//! Command-line boundary for the certification lab.
//!
//! The boundary deliberately prints only positive-schema summaries and stable
//! error classes. Arbitrary `anyhow` chains, paths, URLs, remote bodies, and
//! credential metadata never cross into terminal output.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use grokptah_agent_bridge::PersistentAgentCapture;
use serde::Serialize;
use sha2::Digest;
use uuid::Uuid;

use crate::artifact::{verify_completed_campaign, SafeOutputRoot, DEFAULT_OUTPUT_RELATIVE_PATH};
use crate::authority_stage3::{
    inspect as inspect_authority_stage3, run as run_authority_stage3, AuthorityStage3Options,
    AUTHORITY_STAGE3_OUTPUT_RELATIVE_PATH, AUTHORITY_STAGE3_REPORT_SCHEMA,
};
use crate::manifest::CampaignManifest;
use crate::memory_stage5::{
    inspect as inspect_memory_stage5, run as run_memory_stage5, MemoryStage5Options,
    MEMORY_STAGE5_OUTPUT_RELATIVE_PATH, MEMORY_STAGE5_REPORT_SCHEMA,
};
use crate::normalize::{normalize_capture, replay_fixture, ReplayFixture, MAX_FIXTURE_BYTES};
use crate::report::{
    CampaignReport, DiagnosticCode, FailureClass, ProbeStatus, ReportSummary, RuntimeMode,
    MAX_REPORT_BYTES,
};
use crate::review_manifest::default_campaign_path;
use crate::review_report::{
    inspect_review_campaign, QualityClaim, ReviewMode, ReviewReport, ReviewVerdict,
};
use crate::review_runner::{
    preflight_review, run_review, stderr_progress, ReviewOptions, REVIEW_OUTPUT_RELATIVE_PATH,
};
use crate::runner::{
    default_options, preflight, run_campaign, validate_output_location, CampaignCompletion,
    CampaignOptions, TransportConfig, ATTACH_TOKEN_ENV, DEFAULT_SMOKE_PROBES,
};
use crate::{LAB_REPORT_SCHEMA, REVIEW_REPORT_SCHEMA};

const DEFAULT_ARTIFACT_BUDGET_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(
    name = "grokptah-cert",
    about = "Bounded black-box certification for GrokPtah persistent Agents",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate configuration, service reachability, and MCP capabilities.
    Preflight(CampaignArgs),
    /// Execute a bounded campaign and atomically write its evidence report.
    Run(CampaignArgs),
    /// Validate and summarize a completed campaign report.
    #[command(visible_alias = "report")]
    Inspect(InspectArgs),
    /// Convert one reviewed capture into a structural replay candidate.
    Normalize(NormalizeArgs),
    /// Validate and replay a deterministic structural fixture offline.
    Replay(ReplayArgs),
    /// Explicitly prune old, completed, sentinel-owned campaign directories.
    Prune(PruneArgs),
    /// Validate the checked-in manifest and authoritative catalog binding.
    ValidateManifest(RepositoryArgs),
    /// Run the sealed code-review benchmark contract (fake) or fail closed (live).
    Review(ReviewArgs),
    /// Run the exact-head Stage 5 durable-memory certification campaign.
    Memory(MemoryArgs),
    /// Run the exact-head Stage 3 least-privilege authority campaign.
    Authority(AuthorityArgs),
}

#[derive(Debug, Clone, Args)]
pub struct RepositoryArgs {
    /// Absolute or current-directory-relative repository root.
    #[arg(long, default_value = ".")]
    pub repository: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct CampaignArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute manifest path; defaults to the checked-in overlay.
    #[arg(long)]
    pub manifest: Option<PathBuf>,

    /// Absolute artifact root; defaults to the precise ignored lab root.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Stable subordinate probe ID. Repeat to select more than one.
    #[arg(long = "probe")]
    pub probes: Vec<String>,

    /// Select every manifest probe. Unavailable probes remain honest skips.
    #[arg(long, conflicts_with = "probes")]
    pub all: bool,

    /// Explicitly request a live provider campaign.
    #[arg(long)]
    pub live: bool,

    /// Explicit public grok-* model. Required for local live campaigns.
    #[arg(long)]
    pub model: Option<String>,

    /// Attach to an existing service base URL instead of launching locally.
    #[arg(long)]
    pub attach: Option<String>,

    /// Name of the environment variable containing the attach bearer token.
    #[arg(long, default_value = ATTACH_TOKEN_ENV)]
    pub token_env: String,

    /// Exact service workspace scope. Its value is never reported.
    #[arg(long)]
    pub workspace: Option<String>,

    /// Request mutating attach probes. Current main refuses this safely.
    #[arg(long, requires = "attach")]
    pub allow_attach_mutations: bool,

    /// Acknowledge that the attached target is disposable.
    #[arg(long, requires = "allow_attach_mutations")]
    pub acknowledge_disposable_target: bool,

    /// Total bytes available to one campaign, including interrupted temps.
    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=2_147_483_648)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Args)]
pub struct MemoryArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute artifact root; defaults to the ignored Stage 5 run root.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Total bytes available to the sealed Stage 5 campaign.
    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=2_147_483_648)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Args)]
pub struct AuthorityArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute artifact root; defaults to the ignored Stage 3 run root.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Total bytes available to the sealed Stage 3 campaign.
    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=2_147_483_648)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Args)]
pub struct InspectArgs {
    /// Absolute completed campaign directory containing report.json.
    #[arg(long)]
    pub campaign: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ReviewArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute campaign path; defaults to the checked-in review overlay.
    #[arg(long)]
    pub campaign: Option<PathBuf>,

    /// Absolute artifact root; defaults to the review ignored lab root.
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Explicit live mode. Fails closed without unimplemented enterprise leases.
    #[arg(long)]
    pub live: bool,

    /// Validate the review campaign without executing arms or sealing artifacts.
    #[arg(long)]
    pub preflight: bool,

    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=134217728)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Args)]
pub struct NormalizeArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute path to a bounded PersistentAgentCapture JSON artifact.
    #[arg(long)]
    pub capture: PathBuf,

    /// Absolute sentinel-owned output root for the review candidate.
    #[arg(long)]
    pub output: PathBuf,

    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=2_147_483_648)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// Absolute path to a structural replay fixture.
    #[arg(long)]
    pub fixture: PathBuf,
}

#[derive(Debug, Args)]
pub struct PruneArgs {
    #[command(flatten)]
    pub repository: RepositoryArgs,

    /// Absolute sentinel-owned certification output root.
    #[arg(long)]
    pub output: PathBuf,

    /// Number of newest completed campaigns to retain; never zero.
    #[arg(
        long,
        default_value_t = 5,
        value_parser = parse_positive_retention
    )]
    pub keep: usize,

    #[arg(
        long,
        default_value_t = DEFAULT_ARTIFACT_BUDGET_BYTES,
        value_parser = clap::value_parser!(u64).range(1..=2_147_483_648)
    )]
    pub artifact_budget_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitClass {
    Passed = 0,
    OracleFailure = 1,
    InvalidConfiguration = 2,
    Indeterminate = 3,
    SafetyRefusal = 4,
    GracefulInterruption = 5,
    HarnessFailure = 6,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ManifestSummary {
    valid: bool,
    campaign_id: String,
    probes: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct InspectSummary {
    valid: bool,
    completion_seal_verified: bool,
    certified: bool,
    campaign_id: String,
    summary: ReportSummary,
    failures: Vec<InspectFailure>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct InspectFailure {
    probe_id: String,
    status: ProbeStatus,
    failure_class: FailureClass,
    diagnostics: Vec<DiagnosticCode>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct ReviewInspectSummary {
    valid: bool,
    completion_seal_verified: bool,
    quality_claim_eligible: bool,
    fake_cannot_prove_quality: bool,
    verdict: ReviewVerdict,
    campaign_id: String,
    notice: QualityClaim,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct NormalizeSummary {
    candidate_id: String,
    fixture_sha256: String,
    oracle_sha256: String,
    automatic_promotion: bool,
    human_review_required: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
struct PruneOutput {
    removed: usize,
    retained: usize,
    preserved: usize,
}

/// Execute one parsed command, emit only bounded positive-schema output, and
/// return the typed process exit class.
pub async fn execute(cli: Cli) -> ExitClass {
    match dispatch(cli.command).await {
        Ok((exit, value)) => {
            if serde_json::to_writer(std::io::stdout().lock(), &value).is_err() {
                eprintln!("grokptah-cert: harness_output_failed");
                return ExitClass::HarnessFailure;
            }
            println!();
            exit
        }
        Err(error) => {
            let (exit, code) = classify_error(&error);
            eprintln!("grokptah-cert: {code}");
            exit
        }
    }
}

async fn dispatch(command: Command) -> Result<(ExitClass, serde_json::Value)> {
    match command {
        Command::Preflight(args) => {
            let options = campaign_options(args)?;
            let summary = preflight(&options).await?;
            let exit =
                if summary.unsupported_probe_count > 0 || summary.indeterminate_probe_count > 0 {
                    ExitClass::Indeterminate
                } else {
                    ExitClass::Passed
                };
            Ok((exit, serde_json::to_value(summary)?))
        }
        Command::Run(args) => {
            let options = campaign_options(args)?;
            tokio::select! {
                result = run_campaign(&options) => {
                    let completion = result?;
                    let exit = exit_for_completion(&completion);
                    Ok((exit, serde_json::to_value(completion)?))
                }
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(|_| anyhow::anyhow!("interrupt_handler_failed"))?;
                    Ok((ExitClass::GracefulInterruption, serde_json::json!({
                        "interrupted": true,
                        "resume_supported": false,
                        "artifact_state": "recoverable_if_campaign_initialized"
                    })))
                }
            }
        }
        Command::Inspect(args) => {
            require_absolute(&args.campaign, "campaign_path_must_be_absolute")?;
            let sealed = verify_completed_campaign(&args.campaign)?;
            if sealed.relative_path != "report.json" {
                bail!("campaign seal does not reference a bounded report");
            }
            let bytes = read_regular_bounded(&args.campaign.join("report.json"), MAX_REPORT_BYTES)?;
            if bytes.len() as u64 != sealed.bytes
                || format!("{:x}", sha2::Sha256::digest(&bytes)) != sealed.sha256
            {
                bail!("sealed report does not match its completion record");
            }
            let schema = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| {
                    value
                        .get("schema")
                        .and_then(|schema| schema.as_str().map(str::to_owned))
                });
            if schema.as_deref() == Some(REVIEW_REPORT_SCHEMA) {
                let report = inspect_review_campaign(&args.campaign)?;
                let exit = exit_for_review(&report);
                let summary = ReviewInspectSummary {
                    valid: true,
                    completion_seal_verified: true,
                    quality_claim_eligible: report.quality_claim_eligible,
                    fake_cannot_prove_quality: report.quality_claim
                        == QualityClaim::FakeCannotProveQuality,
                    verdict: report.verdict,
                    campaign_id: report.campaign_id,
                    notice: report.quality_claim,
                };
                return Ok((exit, serde_json::to_value(summary)?));
            }
            if schema.as_deref() == Some(MEMORY_STAGE5_REPORT_SCHEMA) {
                let summary = inspect_memory_stage5(&args.campaign)?;
                let exit = if summary.certification_ready {
                    ExitClass::Passed
                } else {
                    ExitClass::OracleFailure
                };
                return Ok((exit, serde_json::to_value(summary)?));
            }
            if schema.as_deref() == Some(AUTHORITY_STAGE3_REPORT_SCHEMA) {
                let summary = inspect_authority_stage3(&args.campaign)?;
                let exit = if summary.certification_ready {
                    ExitClass::Passed
                } else {
                    ExitClass::OracleFailure
                };
                return Ok((exit, serde_json::to_value(summary)?));
            }
            if sealed.bytes > MAX_REPORT_BYTES as u64 {
                bail!("campaign seal does not reference a bounded report");
            }
            let report: CampaignReport = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("report_json_invalid"))?;
            if report.schema != LAB_REPORT_SCHEMA {
                bail!("report_json_invalid");
            }
            report.validate_at(&args.campaign)?;
            let exit = exit_for_report(&report);
            let failures = report
                .probes
                .iter()
                .filter(|probe| probe.status != ProbeStatus::Passed)
                .map(|probe| InspectFailure {
                    probe_id: probe.probe_id.clone(),
                    status: probe.status,
                    failure_class: probe.failure_class,
                    diagnostics: probe.diagnostics.clone(),
                })
                .collect();
            let summary = InspectSummary {
                valid: true,
                completion_seal_verified: true,
                certified: report.certified,
                campaign_id: report.campaign_id,
                summary: report.summary,
                failures,
            };
            Ok((exit, serde_json::to_value(summary)?))
        }
        Command::Normalize(args) => normalize(args),
        Command::Replay(args) => replay(args),
        Command::Prune(args) => prune(args),
        Command::ValidateManifest(args) => {
            let repository = canonical_repository(&args.repository)?;
            let manifest_path = repository.join("evals/certification-lab/campaign.v1.json");
            let manifest = CampaignManifest::load_checked(&manifest_path, &repository)?;
            let summary = ManifestSummary {
                valid: true,
                campaign_id: manifest.campaign_id,
                probes: manifest.probes.len(),
            };
            Ok((ExitClass::Passed, serde_json::to_value(summary)?))
        }
        Command::Review(args) => review(args).await,
        Command::Memory(args) => memory(args),
        Command::Authority(args) => authority(args),
    }
}

fn authority(args: AuthorityArgs) -> Result<(ExitClass, serde_json::Value)> {
    let repository = canonical_repository(&args.repository.repository)?;
    let output_root = match args.output {
        Some(path) => {
            require_absolute(&path, "output_path_must_be_absolute")?;
            path
        }
        None => repository.join(AUTHORITY_STAGE3_OUTPUT_RELATIVE_PATH),
    };
    let completion = run_authority_stage3(&AuthorityStage3Options {
        repository_root: repository,
        output_root,
        artifact_budget_bytes: args.artifact_budget_bytes,
    })?;
    let exit = if completion.certification_ready {
        ExitClass::Passed
    } else {
        ExitClass::OracleFailure
    };
    Ok((exit, serde_json::to_value(completion)?))
}

fn memory(args: MemoryArgs) -> Result<(ExitClass, serde_json::Value)> {
    let repository = canonical_repository(&args.repository.repository)?;
    let output_root = match args.output {
        Some(path) => {
            require_absolute(&path, "output_path_must_be_absolute")?;
            path
        }
        None => repository.join(MEMORY_STAGE5_OUTPUT_RELATIVE_PATH),
    };
    let completion = run_memory_stage5(&MemoryStage5Options {
        repository_root: repository,
        output_root,
        artifact_budget_bytes: args.artifact_budget_bytes,
    })?;
    let exit = if completion.certification_ready {
        ExitClass::Passed
    } else {
        ExitClass::OracleFailure
    };
    Ok((exit, serde_json::to_value(completion)?))
}

async fn review(args: ReviewArgs) -> Result<(ExitClass, serde_json::Value)> {
    let repository = canonical_repository(&args.repository.repository)?;
    let options = ReviewOptions {
        repository_root: repository.clone(),
        campaign_path: match args.campaign {
            Some(path) => {
                require_absolute(&path, "campaign_path_must_be_absolute")?;
                path
            }
            None => default_campaign_path(&repository),
        },
        output_root: match args.output {
            Some(path) => {
                require_absolute(&path, "output_path_must_be_absolute")?;
                path
            }
            None => repository.join(REVIEW_OUTPUT_RELATIVE_PATH),
        },
        artifact_budget_bytes: args.artifact_budget_bytes,
        mode: if args.live {
            ReviewMode::Live
        } else {
            ReviewMode::Fake
        },
        preflight_only: args.preflight,
    };
    if args.preflight {
        stderr_progress(options.mode, "review_preflight");
        let summary = preflight_review(&options)?;
        let exit = match options.mode {
            ReviewMode::Fake => ExitClass::Passed,
            ReviewMode::Live => ExitClass::Indeterminate,
        };
        return Ok((exit, serde_json::to_value(summary)?));
    }
    stderr_progress(options.mode, "review_start");
    let completion = run_review(&options).await?;
    stderr_progress(options.mode, "review_complete");
    Ok((
        exit_for_review_verdict(completion.verdict),
        serde_json::to_value(completion)?,
    ))
}

fn exit_for_review(report: &ReviewReport) -> ExitClass {
    exit_for_review_verdict(report.verdict)
}

fn exit_for_review_verdict(verdict: ReviewVerdict) -> ExitClass {
    match verdict {
        ReviewVerdict::ContractPassed => ExitClass::Passed,
        ReviewVerdict::Failed => ExitClass::OracleFailure,
        ReviewVerdict::Indeterminate => ExitClass::Indeterminate,
        ReviewVerdict::SafetyFailure => ExitClass::SafetyRefusal,
    }
}

fn campaign_options(args: CampaignArgs) -> Result<CampaignOptions> {
    let repository = canonical_repository(&args.repository.repository)?;
    let mut options = default_options(&repository);
    options.manifest_path = match args.manifest {
        Some(path) => {
            require_absolute(&path, "manifest_path_must_be_absolute")?;
            path
        }
        None => repository.join("evals/certification-lab/campaign.v1.json"),
    };
    options.output_root = match args.output {
        Some(path) => {
            require_absolute(&path, "output_path_must_be_absolute")?;
            path
        }
        None => repository.join(DEFAULT_OUTPUT_RELATIVE_PATH),
    };
    options.runtime_mode = if args.live {
        RuntimeMode::Live
    } else {
        RuntimeMode::Offline
    };
    options.live_model_explicit = args.model.is_some();
    if let Some(model) = args.model {
        options.model = model;
    }
    options.artifact_budget_bytes = args.artifact_budget_bytes;
    options.selected_probe_ids = if args.all {
        Vec::new()
    } else if args.probes.is_empty() {
        DEFAULT_SMOKE_PROBES
            .iter()
            .map(|id| (*id).to_owned())
            .collect()
    } else {
        args.probes
    };
    options.transport = match args.attach {
        Some(endpoint) => TransportConfig::Attach {
            endpoint,
            token_env: args.token_env,
            workspace: args.workspace,
            allow_mutations: args.allow_attach_mutations,
            disposable_target_acknowledged: args.acknowledge_disposable_target,
        },
        None => {
            if args.workspace.is_some()
                || args.allow_attach_mutations
                || args.acknowledge_disposable_target
                || args.token_env != ATTACH_TOKEN_ENV
            {
                bail!("attach_only_option_without_attach");
            }
            TransportConfig::Local
        }
    };
    Ok(options)
}

fn normalize(args: NormalizeArgs) -> Result<(ExitClass, serde_json::Value)> {
    require_absolute(&args.capture, "capture_path_must_be_absolute")?;
    require_absolute(&args.output, "output_path_must_be_absolute")?;
    let repository = canonical_repository(&args.repository.repository)?;
    validate_output_location(&args.output, &repository)?;
    let bytes = read_regular_bounded(&args.capture, MAX_FIXTURE_BYTES)?;
    let capture: PersistentAgentCapture =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("capture_json_invalid"))?;
    let candidate = normalize_capture(&capture)?;
    let candidate_bytes = crate::normalize::validate_candidate(&candidate)?;
    let output = SafeOutputRoot::open(&args.output, &repository, None, args.artifact_budget_bytes)?;
    let campaign_id = format!("normalize-{}", &Uuid::new_v4().simple().to_string()[..16]);
    let artifacts = output.create_campaign(&campaign_id)?;
    let candidate_digest = artifacts.write_final("review-candidate.json", &candidate_bytes)?;
    let installed = artifacts.bounded_read("review-candidate.json", MAX_FIXTURE_BYTES as u64)?;
    let installed_candidate: crate::normalize::ReviewCandidate = serde_json::from_slice(&installed)
        .map_err(|_| anyhow::anyhow!("installed_candidate_invalid"))?;
    crate::normalize::validate_candidate(&installed_candidate)?;
    artifacts.mark_complete(&candidate_digest)?;
    let summary = NormalizeSummary {
        candidate_id: candidate.candidate_id,
        fixture_sha256: candidate.promotion_manifest.fixture_sha256,
        oracle_sha256: candidate.promotion_manifest.oracle_sha256,
        automatic_promotion: false,
        human_review_required: true,
    };
    Ok((ExitClass::Passed, serde_json::to_value(summary)?))
}

fn replay(args: ReplayArgs) -> Result<(ExitClass, serde_json::Value)> {
    require_absolute(&args.fixture, "fixture_path_must_be_absolute")?;
    let bytes = read_regular_bounded(&args.fixture, MAX_FIXTURE_BYTES)?;
    let fixture: ReplayFixture =
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("fixture_json_invalid"))?;
    let result = replay_fixture(&fixture)?;
    let exit = if result.passed {
        ExitClass::Passed
    } else {
        ExitClass::OracleFailure
    };
    Ok((exit, serde_json::to_value(result)?))
}

fn prune(args: PruneArgs) -> Result<(ExitClass, serde_json::Value)> {
    require_absolute(&args.output, "output_path_must_be_absolute")?;
    let repository = canonical_repository(&args.repository.repository)?;
    validate_output_location(&args.output, &repository)?;
    let output = SafeOutputRoot::open(&args.output, &repository, None, args.artifact_budget_bytes)?;
    let result = output.prune_completed(args.keep, None)?;
    let summary = PruneOutput {
        removed: result.removed,
        retained: result.retained,
        preserved: result.preserved,
    };
    Ok((ExitClass::Passed, serde_json::to_value(summary)?))
}

fn exit_for_summary(summary: &ReportSummary) -> ExitClass {
    if summary.failed > 0 {
        ExitClass::OracleFailure
    } else if summary.probes == 0
        || summary.passed != summary.probes
        || summary.skipped > 0
        || summary.indeterminate > 0
    {
        ExitClass::Indeterminate
    } else {
        ExitClass::Passed
    }
}

fn exit_for_certification(summary: &ReportSummary, certified: bool) -> ExitClass {
    let status = exit_for_summary(summary);
    if status == ExitClass::Passed && !certified {
        ExitClass::Indeterminate
    } else {
        status
    }
}

fn exit_for_completion(completion: &CampaignCompletion) -> ExitClass {
    exit_for_evidence(
        &completion.summary,
        completion.certified,
        &completion.failure_classes,
    )
}

fn exit_for_report(report: &CampaignReport) -> ExitClass {
    let mut classes = Vec::new();
    for probe in &report.probes {
        if probe.failure_class != FailureClass::None && !classes.contains(&probe.failure_class) {
            classes.push(probe.failure_class);
        }
    }
    exit_for_evidence(&report.summary, report.certified, &classes)
}

fn exit_for_evidence(
    summary: &ReportSummary,
    certified: bool,
    failure_classes: &[FailureClass],
) -> ExitClass {
    if failure_classes.contains(&FailureClass::Safety) {
        ExitClass::SafetyRefusal
    } else if failure_classes.contains(&FailureClass::Interrupted) {
        ExitClass::GracefulInterruption
    } else if failure_classes.contains(&FailureClass::Transport) {
        ExitClass::HarnessFailure
    } else if failure_classes.contains(&FailureClass::Authentication) {
        ExitClass::Indeterminate
    } else {
        exit_for_certification(summary, certified)
    }
}

fn canonical_repository(path: &Path) -> Result<PathBuf> {
    let canonical = dunce::canonicalize(path).map_err(|_| anyhow::anyhow!("repository_invalid"))?;
    if !canonical.is_dir() {
        bail!("repository_invalid");
    }
    Ok(canonical)
}

fn require_absolute(path: &Path, code: &'static str) -> Result<()> {
    if !path.is_absolute() {
        bail!(code);
    }
    Ok(())
}

fn parse_positive_retention(value: &str) -> std::result::Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| "retention must be a positive integer".to_owned())?;
    if parsed == 0 {
        return Err("retention must be at least one".to_owned());
    }
    Ok(parsed)
}

fn read_regular_bounded(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("artifact_unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("artifact_not_regular");
    }
    let max_u64 = u64::try_from(maximum).context("artifact bound conversion")?;
    if metadata.len() == 0 || metadata.len() > max_u64 {
        bail!("artifact_size_out_of_bounds");
    }
    let mut file = File::open(path).map_err(|_| anyhow::anyhow!("artifact_open_failed"))?;
    let opened = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("artifact_metadata_failed"))?;
    if !opened.is_file() || opened.len() != metadata.len() || !same_file(&metadata, &opened) {
        bail!("artifact_changed_during_open");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(max_u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| anyhow::anyhow!("artifact_read_failed"))?;
    if bytes.is_empty() || bytes.len() > maximum {
        bail!("artifact_size_out_of_bounds");
    }
    let finished = file
        .metadata()
        .map_err(|_| anyhow::anyhow!("artifact_metadata_failed"))?;
    let current =
        fs::symlink_metadata(path).map_err(|_| anyhow::anyhow!("artifact_changed_during_read"))?;
    if finished.len() != opened.len()
        || current.len() != opened.len()
        || !same_file(&opened, &finished)
        || !same_file(&opened, &current)
    {
        bail!("artifact_changed_during_read");
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

fn classify_error(error: &anyhow::Error) -> (ExitClass, &'static str) {
    let messages: Vec<String> = error.chain().map(ToString::to_string).collect();
    let has = |needle: &str| messages.iter().any(|message| message.contains(needle));
    if has("campaign_completion_tampered") {
        (ExitClass::SafetyRefusal, "campaign_tampered")
    } else if [
        "live_oidc_route_attestation_unavailable",
        "attach_mutation_lease_unavailable",
        "attach_endpoint_",
        "output_path_not_in_precise_ignored_root",
        "output root",
        "free disk",
        "symbolic link",
        "artifact_changed",
        "artifact_not_regular",
        "forbidden-data",
        "redaction",
        "tamper",
        "wrong_service_fingerprint",
        "live_ambient_route_or_credential_override_present",
        "enterprise_gateway_live_attach_must_remain_unimplemented",
    ]
    .iter()
    .any(|needle| has(needle))
    {
        (ExitClass::SafetyRefusal, "safety_refusal")
    } else if has("campaign_not_complete") {
        (ExitClass::Indeterminate, "campaign_incomplete")
    } else if has("live_oidc_attestation_oidc_session_expires_too_soon") {
        (ExitClass::Indeterminate, "live_authentication_unavailable")
    } else if ["mcp_initialize_failed", "tool_discovery_failed"]
        .iter()
        .any(|needle| has(needle))
    {
        (ExitClass::HarnessFailure, "mcp_transport_failure")
    } else if has("start isolated GrokPtah host") {
        (ExitClass::HarnessFailure, "local_host_start_failed")
    } else if has("open isolated durable orchestration store") {
        (ExitClass::HarnessFailure, "local_store_open_failed")
    } else if has("bind isolated loopback MCP control plane") {
        let code = if has("Operation not permitted") || has("Permission denied") {
            "local_control_bind_sandbox_refused"
        } else if has("Address already in use") {
            "local_control_bind_collision"
        } else {
            "local_control_bind_failed"
        };
        (ExitClass::HarnessFailure, code)
    } else if has("reopen disposable GrokPtah runtime home") {
        (ExitClass::HarnessFailure, "local_runtime_home_failed")
    } else if [
        "disposable certification layout",
        "disposable scenario workspace",
        "runtime home",
        "control plane",
        "control server",
    ]
    .iter()
    .any(|needle| has(needle))
    {
        (ExitClass::HarnessFailure, "local_bootstrap_failure")
    } else if [
        "manifest",
        "catalog",
        "_invalid",
        "must_be_absolute",
        "double_opt_in",
        "live_model_must_be_explicit",
        "probe selection",
        "unknown probe",
    ]
    .iter()
    .any(|needle| has(needle))
    {
        (ExitClass::InvalidConfiguration, "invalid_configuration")
    } else if [
        "unavailable",
        "unsupported",
        "authentication",
        "provider_observer",
    ]
    .iter()
    .any(|needle| has(needle))
    {
        (
            ExitClass::Indeterminate,
            "capability_or_evidence_unavailable",
        )
    } else {
        (ExitClass::HarnessFailure, "harness_failure")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_self_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn token_is_only_accepted_by_environment_name() {
        let parsed = Cli::try_parse_from([
            "grokptah-cert",
            "preflight",
            "--attach",
            "https://service.example",
            "--token-env",
            "CERT_TOKEN",
        ])
        .unwrap();
        let Command::Preflight(args) = parsed.command else {
            panic!("wrong command");
        };
        assert_eq!(args.token_env, "CERT_TOKEN");
    }

    #[test]
    fn mutation_ack_requires_mutation_flag() {
        assert!(Cli::try_parse_from([
            "grokptah-cert",
            "run",
            "--attach",
            "https://service.example",
            "--acknowledge-disposable-target",
        ])
        .is_err());
    }

    #[test]
    fn review_command_defaults_to_fake_contract_mode() {
        let parsed = Cli::try_parse_from(["grokptah-cert", "review"]).unwrap();
        let Command::Review(args) = parsed.command else {
            panic!("wrong command");
        };
        assert!(!args.live);
        assert!(!args.preflight);
    }

    #[test]
    fn all_skipped_is_never_success() {
        let summary = ReportSummary {
            probes: 2,
            supported: 0,
            passed: 0,
            failed: 0,
            skipped: 2,
            indeterminate: 0,
        };
        assert_eq!(exit_for_summary(&summary), ExitClass::Indeterminate);
    }

    #[test]
    fn evidence_exit_classes_preserve_transport_and_safety_meanings() {
        let failed = ReportSummary {
            probes: 1,
            supported: 1,
            passed: 0,
            failed: 1,
            skipped: 0,
            indeterminate: 0,
        };
        assert_eq!(
            exit_for_evidence(&failed, false, &[FailureClass::Oracle]),
            ExitClass::OracleFailure
        );
        assert_eq!(
            exit_for_evidence(&failed, false, &[FailureClass::Transport]),
            ExitClass::HarnessFailure
        );
        assert_eq!(
            exit_for_evidence(
                &failed,
                false,
                &[FailureClass::Transport, FailureClass::Safety]
            ),
            ExitClass::SafetyRefusal
        );
        assert_eq!(
            exit_for_evidence(&failed, false, &[FailureClass::Authentication]),
            ExitClass::Indeterminate
        );
    }

    #[test]
    fn error_boundary_never_returns_raw_error_text() {
        let secret = anyhow::anyhow!("remote body Bearer secret-secret-secret");
        let (_, code) = classify_error(&secret);
        assert_eq!(code, "harness_failure");
        assert!(!code.contains("secret"));
    }

    #[test]
    fn incomplete_and_tampered_campaigns_have_distinct_safe_exits() {
        assert_eq!(
            classify_error(&anyhow::anyhow!("campaign_not_complete")),
            (ExitClass::Indeterminate, "campaign_incomplete")
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("campaign_completion_tampered")),
            (ExitClass::SafetyRefusal, "campaign_tampered")
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("attach_endpoint_invalid")),
            (ExitClass::SafetyRefusal, "safety_refusal")
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!("artifact_changed_during_read")),
            (ExitClass::SafetyRefusal, "safety_refusal")
        );
        assert_eq!(
            classify_error(&anyhow::anyhow!(
                "live_oidc_attestation_oidc_session_expires_too_soon"
            )),
            (ExitClass::Indeterminate, "live_authentication_unavailable")
        );
    }

    #[cfg(unix)]
    #[test]
    fn bounded_cli_input_rejects_a_symbolic_link() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("fixture.json");
        let linked = directory.path().join("linked.json");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(read_regular_bounded(&linked, 16).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn default_cli_preflight_arguments_are_executable_offline() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let cli = Cli::try_parse_from([
            "grokptah-cert",
            "preflight",
            "--repository",
            repository.to_str().unwrap(),
        ])
        .unwrap();
        let Command::Preflight(args) = cli.command else {
            panic!("wrong command");
        };
        let options = campaign_options(args).unwrap();
        preflight(&options).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_cli_boundary_returns_success_for_offline_preflight() {
        if !crate::local_service::loopback_test_available() {
            return;
        }
        let repository =
            dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")).unwrap();
        let cli = Cli::try_parse_from([
            "grokptah-cert",
            "preflight",
            "--repository",
            repository.to_str().unwrap(),
        ])
        .unwrap();
        assert_eq!(execute(cli).await, ExitClass::Passed);
    }
}
