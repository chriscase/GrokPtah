//! Fake-contract and live-fail-closed runner for the code-review benchmark.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use grokptah_agent_bridge::{scan_value_for_forbidden_data, BRIDGE_VERSION};
use serde::Serialize;
use uuid::Uuid;

use crate::artifact::{SafeOutputRoot, DEFAULT_OUTPUT_RELATIVE_PATH};
use crate::local_service::{
    enterprise_review_live_evidence, exercise_review_control_plane, git_ref_snapshot,
    workspace_merkle_root, GitRefSnapshot,
};
use crate::review_manifest::{
    default_campaign_path, digest, manifest_source_digest, opaque_id, report_source_digest,
    runner_digest, scorer_digest, ArmId, ArmScript, ArtifactRole, HiddenOracleSet, LiveThresholds,
    MaliciousCall, ReviewAction, ReviewBundle, ReviewCampaign, ReviewOracle, ReviewTool,
    RuntimeCase, RuntimeCorpus, RuntimeFamily, BUNDLED_REVIEW_CAMPAIGN, BUNDLED_REVIEW_CORPUS,
    BUNDLED_REVIEW_FAKE_PROVIDER, EXPECTED_CASE_COUNT, EXPECTED_FAMILY_COUNT, RUNNER_IDENTITY,
    SCORER_IDENTITY,
};
use crate::review_report::{
    arm_metrics, cis_from, deltas_from, evaluate_verdict, inspect_review_campaign, wins_from,
    ActualBounds, Cardinalities, Completeness, IndeterminateReason, OpaqueBinding,
    PublicArtifactRef, QualityClaim, ReviewFingerprint, ReviewImplementationIdentity, ReviewMode,
    ReviewReport, ReviewRuntimeKind, ReviewVerdict, WorkspaceHashes, MAX_REVIEW_REPORT_BYTES,
};
use crate::review_score::{findings_digest, score_paired, ArmCost, ScoredFinding};
use crate::{REVIEW_FINGERPRINT_SCHEMA, REVIEW_IMPLEMENTATION_SCHEMA, REVIEW_REPORT_SCHEMA};

pub const REVIEW_OUTPUT_RELATIVE_PATH: &str = "evals/runs/code-review-benchmark";
const LIVE_ROUTE_OVERRIDE_ENVS: &[&str] =
    &["XAI_API_KEY", "XAI_API_BASE", "GROKPTAH_TOKEN_COMMAND"];
const ADVERSARIAL_REQUEST_TOKENS: u64 = 40;
const BASELINE_NONCE_SEED: &[u8] = b"arm-baseline-v1";
const GROKPTAH_NONCE_SEED: &[u8] = b"arm-grokptah-v1";

#[derive(Debug, Clone)]
pub struct ReviewOptions {
    pub repository_root: PathBuf,
    pub campaign_path: PathBuf,
    pub output_root: PathBuf,
    pub artifact_budget_bytes: u64,
    pub mode: ReviewMode,
    pub preflight_only: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ReviewPreflight {
    pub manifest_valid: bool,
    pub mode: ReviewMode,
    pub cases: u32,
    pub families: u32,
    pub live_replicates_configured: u32,
    pub quality_claim_eligible: bool,
    pub fake_cannot_prove_quality: bool,
    pub enterprise_gateway_lease: bool,
    pub notice: QualityClaim,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ReviewCompletion {
    pub campaign_id: String,
    pub verdict: ReviewVerdict,
    #[serde(rename = "qualityClaimEligible")]
    pub quality_claim_eligible: bool,
    pub fake_cannot_prove_quality: bool,
    pub report_sha256: String,
    pub notice: QualityClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContractMutation {
    RemoveTrueIssue,
    AddLureFp,
    DuplicateFinding,
    ShiftRegion,
    CorruptCausalAtom,
    InflateConfidence,
    OmitCase,
    DuplicateCase,
    SwapArmLabels,
    SwapArmNonces,
    ChangeCorpusDigest,
    ChangeOracleDigest,
    ChangeScorerDigest,
    MixRouteAttestation,
    MixModelAttestation,
    ExceedBound,
    MutateWorkspace,
    DropFakeTransportObservation,
    ChangeImplementationDigest,
    ChangeRunnerDigest,
    MarkFakeQualityEligible,
}

#[derive(Debug, Clone)]
pub struct EvaluableState {
    pub report: ReviewReport,
    pub oracles: HiddenOracleSet,
    pub baseline_findings: Vec<ScoredFinding>,
    pub grokptah_findings: Vec<ScoredFinding>,
    pub baseline_cost: ArmCost,
    pub grokptah_cost: ArmCost,
    pub scripted_baseline_count: usize,
    pub scripted_grokptah_count: usize,
    pub locked_baseline_findings: String,
    pub locked_grokptah_findings: String,
    pub baseline_route: String,
    pub grokptah_route: String,
    pub baseline_model: String,
    pub grokptah_model: String,
    pub bundle_suite_digest: String,
    pub bundle_oracle_digest: String,
    pub expected_baseline_nonce: String,
    pub expected_grokptah_nonce: String,
    pub expected_implementation_digest: String,
}

pub fn default_review_options(repository_root: &Path) -> ReviewOptions {
    ReviewOptions {
        campaign_path: default_campaign_path(repository_root),
        output_root: repository_root.join(REVIEW_OUTPUT_RELATIVE_PATH),
        artifact_budget_bytes: 128 * 1024 * 1024,
        mode: ReviewMode::Fake,
        preflight_only: false,
        repository_root: repository_root.to_path_buf(),
    }
}

pub fn preflight_review(options: &ReviewOptions) -> Result<ReviewPreflight> {
    validate_review_options(options)?;
    let bundle = ReviewBundle::load_checked(&options.campaign_path, &options.repository_root)?;
    Ok(ReviewPreflight {
        manifest_valid: true,
        mode: options.mode,
        cases: EXPECTED_CASE_COUNT as u32,
        families: EXPECTED_FAMILY_COUNT as u32,
        live_replicates_configured: bundle.campaign.live_replicate_count,
        quality_claim_eligible: false,
        fake_cannot_prove_quality: options.mode == ReviewMode::Fake,
        enterprise_gateway_lease: enterprise_review_live_evidence().is_ok(),
        notice: match options.mode {
            ReviewMode::Fake => QualityClaim::FakeCannotProveQuality,
            ReviewMode::Live => QualityClaim::LiveAttestationMissing,
        },
    })
}

pub async fn run_review(options: &ReviewOptions) -> Result<ReviewCompletion> {
    validate_review_options(options)?;
    if options.preflight_only {
        bail!("preflight_only_run");
    }
    let bundle = ReviewBundle::load_checked(&options.campaign_path, &options.repository_root)?;
    match options.mode {
        ReviewMode::Fake => {
            let (_state, completion) = execute_fake(&bundle, Some(options)).await?;
            completion
                .ok_or_else(|| anyhow::anyhow!("fake review seal did not return a completion"))
        }
        ReviewMode::Live => run_live_fail_closed(options, &bundle),
    }
}

pub async fn run_fake_evaluable(bundle: &ReviewBundle) -> Result<EvaluableState> {
    let (state, _) = execute_fake(bundle, None).await?;
    Ok(state)
}

pub fn inspect_review_output(directory: &Path) -> Result<ReviewReport> {
    inspect_review_campaign(directory)
}

pub fn contract_verdict(state: &EvaluableState) -> ReviewVerdict {
    if state.oracles.issues.len() != EXPECTED_CASE_COUNT
        || state.baseline_findings.len() != state.scripted_baseline_count
        || state.grokptah_findings.len() != state.scripted_grokptah_count
        || findings_digest(&state.baseline_findings) != state.locked_baseline_findings
        || findings_digest(&state.grokptah_findings) != state.locked_grokptah_findings
        || state.report.oracle_digest != state.bundle_oracle_digest
        || state.report.suite_digest != state.bundle_suite_digest
        || state.report.scorer_digest != scorer_digest()
        || state.report.runner_digest != runner_digest()
        || state.report.implementation_digest != state.expected_implementation_digest
        || state.report.runtime_kind != ReviewRuntimeKind::FakeLoopbackTransport
        || state.report.completeness.provider_observation_complete
        || !state
            .report
            .completeness
            .fake_transport_observation_complete
        || state.report.corpus_digest != state.report.binding.corpus_digest
        || state.baseline_route != state.grokptah_route
        || state.baseline_model != state.grokptah_model
        || state.report.binding.baseline_arm_nonce != state.expected_baseline_nonce
        || state.report.binding.grokptah_arm_nonce != state.expected_grokptah_nonce
        || state.report.cardinalities.cases_scored != EXPECTED_CASE_COUNT as u32
    {
        return ReviewVerdict::Failed;
    }
    if let Ok(scored) = score_paired(
        &state.oracles,
        &state.baseline_findings,
        &state.grokptah_findings,
        state.baseline_cost,
        state.grokptah_cost,
        &sealed_thresholds(),
    ) {
        if (scored.grokptah.weighted_utility - state.report.metrics.grokptah.weighted_utility).abs()
            > 1e-9
            || (scored.baseline.weighted_utility - state.report.metrics.baseline.weighted_utility)
                .abs()
                > 1e-9
        {
            return ReviewVerdict::Failed;
        }
    } else {
        return ReviewVerdict::Failed;
    }
    evaluate_verdict(&state.report)
}

pub fn apply_mutation(
    state: &mut EvaluableState,
    mutation: ContractMutation,
) -> Result<ReviewVerdict> {
    match mutation {
        ContractMutation::RemoveTrueIssue => {
            state.oracles.issues.pop();
        }
        ContractMutation::AddLureFp => {
            if let Some(mut lure) = state.grokptah_findings.first().cloned() {
                lure.causal_atom = "benign-comment-style-lure".into();
                lure.confidence_millis = 400;
                state.grokptah_findings.push(lure);
            }
        }
        ContractMutation::DuplicateFinding => {
            if let Some(finding) = state.grokptah_findings.first().cloned() {
                state.grokptah_findings.push(finding);
            }
        }
        ContractMutation::ShiftRegion => {
            if let Some(finding) = state.grokptah_findings.first_mut() {
                finding.region.start_line = finding.region.start_line.saturating_add(40);
                finding.region.end_line = finding.region.end_line.saturating_add(40);
            }
        }
        ContractMutation::CorruptCausalAtom => {
            if let Some(finding) = state.grokptah_findings.first_mut() {
                finding.causal_atom = "corrupted-causal-atom".into();
            }
        }
        ContractMutation::InflateConfidence => {
            for finding in &mut state.grokptah_findings {
                finding.confidence_millis =
                    finding.confidence_millis.saturating_add(200).min(1_000);
            }
        }
        ContractMutation::OmitCase => {
            state.report.cardinalities.cases_scored =
                state.report.cardinalities.cases_scored.saturating_sub(1);
            state.oracles.issues.pop();
        }
        ContractMutation::DuplicateCase => {
            if let Some(issue) = state.oracles.issues.first().cloned() {
                state.oracles.issues.push(issue);
            }
            state.report.cardinalities.cases_scored += 1;
        }
        ContractMutation::SwapArmLabels => {
            std::mem::swap(
                &mut state.report.metrics.baseline,
                &mut state.report.metrics.grokptah,
            );
        }
        ContractMutation::SwapArmNonces => {
            std::mem::swap(
                &mut state.report.binding.baseline_arm_nonce,
                &mut state.report.binding.grokptah_arm_nonce,
            );
        }
        ContractMutation::ChangeCorpusDigest => {
            state.report.corpus_digest = digest(b"mutated-corpus");
        }
        ContractMutation::ChangeOracleDigest => {
            state.report.oracle_digest = digest(b"mutated-oracle");
        }
        ContractMutation::ChangeScorerDigest => {
            state.report.scorer_digest = digest(b"mutated-scorer");
        }
        ContractMutation::MixRouteAttestation => {
            state.grokptah_route = digest(b"drifted-route");
            state.report.binding.attestation_present = true;
            state.report.binding.attestation_valid = false;
        }
        ContractMutation::MixModelAttestation => {
            state.grokptah_model = "opaque-premium-model-v1".into();
            state.report.binding.modest_tier_attested = false;
        }
        ContractMutation::ExceedBound => {
            state.report.bounds_actual.provider_requests =
                state.report.bounds_configured.max_provider_requests + 1;
        }
        ContractMutation::MutateWorkspace => {
            state.report.workspace.post_merkle_root = digest(b"mutated-workspace");
        }
        ContractMutation::DropFakeTransportObservation => {
            state
                .report
                .completeness
                .fake_transport_observation_complete = false;
        }
        ContractMutation::ChangeImplementationDigest => {
            state.report.implementation_digest = digest(b"mutated-implementation");
        }
        ContractMutation::ChangeRunnerDigest => {
            state.report.runner_digest = digest(b"mutated-runner");
        }
        ContractMutation::MarkFakeQualityEligible => {
            state.report.quality_claim_eligible = true;
        }
    }
    Ok(contract_verdict(state))
}

fn sealed_thresholds() -> LiveThresholds {
    LiveThresholds {
        precision: 0.75,
        weighted_recall: 0.75,
        high_critical_recall: 0.85,
        usefulness: 0.70,
        brier: 0.20,
        ece: 0.15,
        paired_weighted_utility_lift: 0.15,
        paired_weighted_utility_lift_ci_lower: 0.08,
        recall_lift: 0.15,
        recall_lift_ci_lower: 0.05,
        family_wins: 6,
        family_count: 8,
        family_max_worse: 0.10,
        token_ratio: 6.0,
        request_ratio: 6.0,
        wall_ratio: 5.0,
    }
}

fn run_live_fail_closed(
    options: &ReviewOptions,
    bundle: &ReviewBundle,
) -> Result<ReviewCompletion> {
    if ambient_override_present() {
        bail!("live_ambient_route_or_credential_override_present");
    }
    // A valid broker lease proves only that admission can be re-established.
    // The runner still emits an indeterminate report until a real provider
    // campaign supplies authoritative usage and paired quality evidence.
    let enterprise_evidence = enterprise_review_live_evidence().ok();
    let enterprise_lease_admitted = enterprise_evidence.is_some();
    let output = SafeOutputRoot::open(
        &options.output_root,
        &options.repository_root,
        None,
        options.artifact_budget_bytes,
    )?;
    let campaign_id = format!("review-live-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let artifacts = output.create_campaign(&campaign_id)?;
    let campaign_digest =
        artifacts.write_final("contract/campaign.json", &bundle.campaign_bytes)?;
    let fingerprint = fingerprint_from_bundle(bundle)?;
    let fingerprint_bytes = serde_json::to_vec_pretty(&fingerprint)?;
    scan_value_for_forbidden_data(&serde_json::to_value(&fingerprint)?)
        .map_err(|_| anyhow!("review fingerprint failed forbidden-data scanning"))?;
    let fingerprint_digest =
        artifacts.write_final("contract/fingerprint.json", &fingerprint_bytes)?;
    let identity_bytes = implementation_bytes(bundle)?;
    let identity_digest = artifacts.write_final("contract/implementation.json", &identity_bytes)?;
    let placeholder = digest(b"unmaterialized-live-review-workspace-v1");
    let mut report = base_report(bundle, ReviewMode::Live);
    report.implementation_digest = identity_digest.sha256.clone();
    if let Some(evidence) = &enterprise_evidence {
        scan_value_for_forbidden_data(&serde_json::to_value(evidence)?)
            .map_err(|_| anyhow!("enterprise review evidence failed forbidden-data scanning"))?;
        report.binding.route_fingerprint = digest(evidence.route_binding_digest.as_bytes());
        report.binding.deployment_fingerprint = digest(evidence.policy_digest.as_bytes());
        report.binding.credential_fingerprint = digest(evidence.lease_id.as_bytes());
        report.binding.model_fingerprint = digest(evidence.model_id.as_bytes());
        report.binding.attestation_present = true;
        report.binding.attestation_valid = true;
        report.binding.modest_tier_attested = true;
        report.binding.premium_fallback_attested_absent = evidence.no_premium_fallback;
        report.binding.egress_attestation_present = evidence.egress_firewall_attested;
    }
    report.completeness.artifacts_consumed = true;
    report.completeness.bounds_consumed = true;
    report.live_indeterminate_reasons = [
        (!enterprise_lease_admitted)
            .then_some(IndeterminateReason::EnterpriseGatewayLeaseUnimplemented),
        (!enterprise_lease_admitted).then_some(IndeterminateReason::DeploymentAttestationMissing),
        (!enterprise_lease_admitted)
            .then_some(IndeterminateReason::EgressFirewallAttestationMissing),
        Some(IndeterminateReason::LiveReplicatesNotExecuted),
        Some(IndeterminateReason::AuthoritativeUsageMissing),
    ]
    .into_iter()
    .flatten()
    .collect();
    report.quality_claim = QualityClaim::LiveAttestationMissing;
    report.forbidden_scan_passed = true;
    report.workspace = WorkspaceHashes {
        pre_merkle_root: placeholder.clone(),
        post_merkle_root: placeholder.clone(),
        pre_git_head: placeholder.clone(),
        post_git_head: placeholder.clone(),
        pre_git_refs: placeholder.clone(),
        post_git_refs: placeholder,
        pre_publication_count: 0,
        post_publication_count: 0,
    };
    report.artifacts = vec![
        public_ref(&campaign_digest, ArtifactRole::SuiteManifest),
        public_ref(&fingerprint_digest, ArtifactRole::DigestFingerprint),
        public_ref(&identity_digest, ArtifactRole::ImplementationIdentity),
    ];
    report.bounds_actual.artifact_bytes = sum_artifact_bytes(&report.artifacts)?;
    report.verdict = evaluate_verdict(&report);
    let report_bytes = report.validate_structure()?;
    let sealed = artifacts.write_final("report.json", &report_bytes)?;
    artifacts.mark_complete(&sealed)?;
    Ok(completion_from_sealed(&report, &sealed.sha256))
}

#[allow(clippy::too_many_lines)]
async fn execute_fake(
    bundle: &ReviewBundle,
    options: Option<&ReviewOptions>,
) -> Result<(EvaluableState, Option<ReviewCompletion>)> {
    let started = Instant::now();
    let mut actions: BTreeSet<ReviewAction> = bundle.campaign.actions.iter().copied().collect();
    let mut oracles: BTreeSet<ReviewOracle> = bundle.campaign.oracles.iter().copied().collect();
    let (runtime, hidden) = bundle.corpus.split()?;
    tick(&mut actions, ReviewAction::MaterializeSyntheticWorkspace)?;
    let layout = tempfile::tempdir().context("create disposable review layout")?;
    let workspace = layout.path().join("workspace");
    let inaccessible = layout.path().join("inaccessible");
    let runtime_home = layout.path().join("runtime-home");
    std::fs::create_dir(&workspace).context("create review workspace")?;
    std::fs::create_dir(&inaccessible).context("create inaccessible canary root")?;
    std::fs::create_dir(&runtime_home).context("create review runtime home")?;
    materialize_workspace(&workspace, &runtime)?;
    std::fs::write(
        inaccessible.join("canary.dat"),
        bundle.corpus.inaccessible_canary.body.as_bytes(),
    )
    .context("write inaccessible canary outside the workspace")?;
    assert_tree_excludes_canary(&workspace, &bundle.corpus.inaccessible_canary.body)?;
    init_git(&workspace)?;
    tick(&mut actions, ReviewAction::SnapshotWorkspacePre)?;
    let pre_merkle = workspace_merkle_root(&workspace)?;
    let pre_git = git_ref_snapshot(&workspace)?;
    tick(&mut actions, ReviewAction::BindPairedArms)?;
    let route = digest(bundle.fake_provider.binding.model_fingerprint.as_bytes());
    let baseline_nonce = digest(BASELINE_NONCE_SEED);
    let grokptah_nonce = digest(GROKPTAH_NONCE_SEED);
    let binding = OpaqueBinding {
        pair_nonce: digest(bundle.campaign_digest.as_bytes()),
        baseline_arm_nonce: baseline_nonce.clone(),
        grokptah_arm_nonce: grokptah_nonce.clone(),
        route_fingerprint: route.clone(),
        deployment_fingerprint: digest(b"opaque-deployment-v1"),
        credential_fingerprint: digest(b"opaque-credential-v1"),
        model_fingerprint: bundle.fake_provider.binding.model_fingerprint.clone(),
        effort: bundle.fake_provider.binding.effort.clone(),
        decoding: bundle.fake_provider.binding.decoding.clone(),
        prompt_cap_bytes: bundle.campaign.prompt_cap_bytes,
        response_cap_bytes: bundle.campaign.response_cap_bytes,
        corpus_digest: bundle.corpus_digest.clone(),
        attestation_present: false,
        attestation_valid: false,
        modest_tier_attested: false,
        premium_fallback_attested_absent: false,
        egress_attestation_present: false,
    };
    tick(&mut oracles, ReviewOracle::PairedBindingIdentical)?;

    let mut loopback = FakeReviewLoopback::new(
        &workspace,
        inaccessible.join("canary.dat"),
        bundle.corpus.inaccessible_canary.body.clone(),
        bundle.corpus.inaccessible_canary.logical_name.clone(),
        route.clone(),
        bundle.fake_provider.quota.window_requests,
    )?;

    let mut baseline_findings = Vec::new();
    let mut grokptah_findings = Vec::new();
    let mut baseline_cost = ArmCost::default();
    let mut grokptah_cost = ArmCost::default();
    let mut canary_evidence_hits = 0u32;
    let mut baseline_max_requests = 0u32;
    let mut grokptah_max_requests = 0u32;
    let mut baseline_max_tokens = 0u64;
    let mut grokptah_max_tokens = 0u64;
    let mut cases_seen = HashSet::new();
    let canary = &bundle.corpus.inaccessible_canary.body;

    tick(&mut actions, ReviewAction::RunBaselineArm)?;
    for family in &runtime.families {
        for case in &family.cases {
            let script = bundle.fake_provider.script(&case.id, ArmId::Baseline)?;
            let (findings, cost, ev_hits) = loopback.run_arm(
                ArmId::Baseline,
                family,
                case,
                script,
                &bundle.campaign,
                canary,
            )?;
            canary_evidence_hits = canary_evidence_hits.saturating_add(ev_hits);
            baseline_max_requests = baseline_max_requests.max(script.requests.len() as u32);
            baseline_max_tokens = baseline_max_tokens.max(cost.authoritative_tokens);
            add_cost(&mut baseline_cost, cost)?;
            baseline_findings.extend(findings);
            cases_seen.insert(case.id.clone());
        }
    }
    tick(&mut oracles, ReviewOracle::BaselineExactlyOneRequest)?;

    tick(&mut actions, ReviewAction::RunGrokptahArm)?;
    for family in &runtime.families {
        for case in &family.cases {
            let script = bundle.fake_provider.script(&case.id, ArmId::Grokptah)?;
            let (findings, cost, ev_hits) = loopback.run_arm(
                ArmId::Grokptah,
                family,
                case,
                script,
                &bundle.campaign,
                canary,
            )?;
            canary_evidence_hits = canary_evidence_hits.saturating_add(ev_hits);
            grokptah_max_requests = grokptah_max_requests.max(script.requests.len() as u32);
            grokptah_max_tokens = grokptah_max_tokens.max(cost.authoritative_tokens);
            add_cost(&mut grokptah_cost, cost)?;
            grokptah_findings.extend(findings);
        }
    }
    tick(&mut oracles, ReviewOracle::GrokptahRequestBound)?;
    tick(&mut oracles, ReviewOracle::NoForbiddenTools)?;
    tick(&mut oracles, ReviewOracle::ScriptedFindingsConsumed)?;
    tick(&mut oracles, ReviewOracle::DeclaredCasesConsumed)?;

    tick(&mut actions, ReviewAction::RestartAfterDurableAdmission)?;
    let findings_before_restart = grokptah_findings.len();
    loopback.begin_in_flight()?;
    loopback.restart_drop_inflight()?;
    if grokptah_findings.len() != findings_before_restart {
        loopback.duplicate_finding_after_restart =
            loopback.duplicate_finding_after_restart.saturating_add(1);
        bail!("restart duplicated grokptah findings");
    }
    tick(&mut oracles, ReviewOracle::RestartNoImplicitResend)?;
    tick(&mut oracles, ReviewOracle::RestartNoDuplicateFinding)?;

    tick(&mut actions, ReviewAction::ObserveRouteDrift)?;
    loopback.begin_in_flight()?;
    loopback.observe_route_drift()?;
    loopback.admit_blocked_on_drift()?;
    loopback.requalify()?;
    loopback.admit_observed(ADVERSARIAL_REQUEST_TOKENS)?;
    tick(&mut oracles, ReviewOracle::RouteDriftFreezesInflight)?;
    tick(&mut oracles, ReviewOracle::RouteDriftBlocksUntilRequalified)?;

    tick(&mut actions, ReviewAction::ObserveQuotaOneUnder)?;
    loopback.quota_one_under()?;
    tick(&mut oracles, ReviewOracle::QuotaOneUnderAdmits)?;

    tick(&mut actions, ReviewAction::ObserveQuotaExhausted)?;
    loopback.quota_exhausted()?;
    tick(&mut oracles, ReviewOracle::QuotaExhaustedBlocks)?;

    tick(&mut actions, ReviewAction::ObserveQuotaWindowAdvance)?;
    loopback.quota_window_advance()?;
    tick(&mut oracles, ReviewOracle::QuotaWindowAdvanceResets)?;

    tick(&mut actions, ReviewAction::DenyMutatorsAndPublish)?;
    for call in &bundle.fake_provider.malicious_calls {
        loopback.dispatch_malicious(call)?;
    }
    let wire_names: Vec<String> = bundle
        .fake_provider
        .malicious_calls
        .iter()
        .map(|call| call.wire_name.clone())
        .collect();
    let mcp = exercise_review_control_plane(&workspace, &runtime_home, &wire_names).await?;
    if mcp.successful_denied_wire_calls > 0 || mcp.forbidden_tools_listed > 0 {
        bail!("review control plane dispatched a denied tool");
    }
    let mcp_publish = u32::try_from(
        bundle
            .fake_provider
            .malicious_calls
            .iter()
            .filter(|call| call.tool == ReviewTool::Publish)
            .count(),
    )?;
    let mcp_mutators = mcp.denied_wire_calls.saturating_sub(mcp_publish);
    loopback.mutator_denials = loopback.mutator_denials.saturating_add(mcp_mutators);
    loopback.publish_denials = loopback.publish_denials.saturating_add(mcp_publish);
    if loopback.mutator_callback_count != 0 || loopback.publish_callback_count != 0 {
        bail!("mutator or publish callback executed");
    }
    tick(&mut oracles, ReviewOracle::MutatorsDenied)?;
    tick(&mut oracles, ReviewOracle::PublishDenied)?;

    tick(&mut actions, ReviewAction::ProveInaccessibleCanaryAbsent)?;
    for probe in &bundle.fake_provider.canary_probes {
        loopback.probe_canary(&probe.logical_name)?;
    }
    if loopback.canary_request_hits != 0 || canary_evidence_hits != 0 {
        bail!("canary payload was disclosed");
    }
    tick(&mut oracles, ReviewOracle::CanaryAbsentFromRequests)?;
    tick(&mut oracles, ReviewOracle::CanaryAbsentFromEvidence)?;

    tick(&mut actions, ReviewAction::SnapshotWorkspacePost)?;
    assert_tree_excludes_canary(&workspace, canary)?;
    let post_merkle = workspace_merkle_root(&workspace)?;
    let post_git = git_ref_snapshot(&workspace)?;
    if pre_merkle != post_merkle {
        bail!("workspace merkle root changed during review");
    }
    if pre_git != post_git {
        bail!("git refs or publication count changed during review");
    }
    tick(&mut oracles, ReviewOracle::WorkspaceMerkleUnchanged)?;
    tick(&mut oracles, ReviewOracle::GitRefsUnchanged)?;
    tick(&mut oracles, ReviewOracle::RemotePublicationUnchanged)?;

    let extra_requests = loopback
        .observed_requests
        .checked_sub(baseline_cost.requests)
        .and_then(|value| value.checked_sub(grokptah_cost.requests))
        .context("fake transport request observation is below scripted admissions")?;
    let extra_tokens = loopback
        .observed_tokens
        .checked_sub(baseline_cost.authoritative_tokens)
        .and_then(|value| value.checked_sub(grokptah_cost.authoritative_tokens))
        .context("fake transport token observation is below scripted admissions")?;
    grokptah_cost.requests = grokptah_cost
        .requests
        .checked_add(extra_requests)
        .context("request overflow")?;
    grokptah_cost.authoritative_tokens = grokptah_cost
        .authoritative_tokens
        .checked_add(extra_tokens)
        .context("token overflow")?;

    tick(&mut actions, ReviewAction::ScoreOneToOne)?;
    let scored = score_paired(
        &hidden,
        &baseline_findings,
        &grokptah_findings,
        baseline_cost,
        grokptah_cost,
        &bundle.campaign.thresholds,
    )?;
    tick(&mut oracles, ReviewOracle::ScorerOneToOne)?;

    if loopback.observed_requests > bundle.campaign.bounds.max_provider_requests
        || loopback.observed_tokens > bundle.campaign.bounds.max_authoritative_tokens
        || baseline_max_requests > 1
        || grokptah_max_requests > 6
        || baseline_max_tokens > 8_000
        || grokptah_max_tokens > 24_000
    {
        bail!("review campaign exceeded a declared bound");
    }
    tick(&mut oracles, ReviewOracle::AuthoritativeUsagePresent)?;
    tick(&mut oracles, ReviewOracle::FakeQualityIneligible)?;
    tick(&mut oracles, ReviewOracle::PublicRedactionPassed)?;
    tick(&mut actions, ReviewAction::SealPublicReport)?;
    if !actions.is_empty() || !oracles.is_empty() {
        bail!("review campaign left declared actions or oracles unconsumed");
    }

    let identity_bytes = implementation_bytes(bundle)?;
    let fingerprint = fingerprint_from_bundle(bundle)?;
    let fingerprint_bytes = serde_json::to_vec_pretty(&fingerprint)?;
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = base_report(bundle, ReviewMode::Fake);
    report.binding = binding;
    report.implementation_digest = digest(&identity_bytes);
    report.bounds_actual = ActualBounds {
        provider_requests: loopback.observed_requests,
        authoritative_tokens: loopback.observed_tokens,
        duration_millis: elapsed,
        continuations: 1,
        artifact_bytes: 0,
        baseline_max_requests_per_case: baseline_max_requests,
        grokptah_max_requests_per_case: grokptah_max_requests,
        baseline_max_tokens_per_case: baseline_max_tokens,
        grokptah_max_tokens_per_case: grokptah_max_tokens,
    };
    report.metrics = arm_metrics(&scored);
    report.deltas = deltas_from(&scored);
    report.cis = cis_from(&scored);
    report.wins = wins_from(&scored);
    report.family_utility = scored.grokptah.family_utility.clone();
    report.cardinalities = Cardinalities {
        restart_count: loopback.restart_count,
        implicit_resend_count: loopback.implicit_resend_count,
        duplicate_finding_after_restart: loopback.duplicate_finding_after_restart,
        route_drift_events: loopback.route_drift_events,
        in_flight_frozen: loopback.in_flight_frozen,
        admissions_blocked_on_drift: loopback.admissions_blocked_on_drift,
        explicit_requalifications: loopback.explicit_requalifications,
        quota_one_under_admitted: loopback.quota_one_under_admitted,
        quota_exhausted_blocked: loopback.quota_exhausted_blocked,
        quota_window_advances: loopback.quota_window_advances,
        mutator_denials: loopback.mutator_denials,
        publish_denials: loopback.publish_denials,
        canary_request_hits: loopback.canary_request_hits,
        canary_evidence_hits,
        canary_address_denials: loopback.canary_address_denials,
        cases_scored: u32::try_from(cases_seen.len()).context("case count")?,
        families_scored: u32::try_from(runtime.families.len()).context("family count")?,
        live_replicates_configured: bundle.campaign.live_replicate_count,
        live_replicates_executed: 0,
    };
    report.workspace = hashes(&pre_merkle, &post_merkle, &pre_git, &post_git);
    report.artifacts = vec![
        PublicArtifactRef {
            relative_path: "contract/campaign.json".into(),
            sha256: bundle.campaign_digest.clone(),
            bytes: u64::try_from(bundle.campaign_bytes.len())?,
            role: ArtifactRole::SuiteManifest,
        },
        PublicArtifactRef {
            relative_path: "contract/fingerprint.json".into(),
            sha256: digest(&fingerprint_bytes),
            bytes: u64::try_from(fingerprint_bytes.len())?,
            role: ArtifactRole::DigestFingerprint,
        },
        PublicArtifactRef {
            relative_path: "contract/implementation.json".into(),
            sha256: digest(&identity_bytes),
            bytes: u64::try_from(identity_bytes.len())?,
            role: ArtifactRole::ImplementationIdentity,
        },
    ];
    report.bounds_actual.artifact_bytes = sum_artifact_bytes(&report.artifacts)?;
    report.completeness = Completeness {
        provider_observation_complete: false,
        fake_transport_observation_complete: loopback.observed_requests > 0
            && loopback.observed_tokens > 0
            && loopback.canary_address_denials > 0,
        authoritative_usage_complete: loopback.observed_tokens > 0,
        egress_attestation_complete: false,
        deployment_attestation_complete: false,
        actions_consumed: true,
        oracles_consumed: true,
        cases_consumed: true,
        artifacts_consumed: true,
        bounds_consumed: true,
    };
    report.quality_claim = QualityClaim::FakeCannotProveQuality;
    report.quality_claim_eligible = false;
    report.forbidden_scan_passed = true;
    report.verdict = ReviewVerdict::ContractPassed;

    let mut state = EvaluableState {
        report,
        oracles: hidden,
        scripted_baseline_count: baseline_findings.len(),
        scripted_grokptah_count: grokptah_findings.len(),
        locked_baseline_findings: findings_digest(&baseline_findings),
        locked_grokptah_findings: findings_digest(&grokptah_findings),
        baseline_findings,
        grokptah_findings,
        baseline_cost,
        grokptah_cost,
        baseline_route: route.clone(),
        grokptah_route: loopback.route.clone(),
        baseline_model: bundle.fake_provider.binding.model_fingerprint.clone(),
        grokptah_model: bundle.fake_provider.binding.model_fingerprint.clone(),
        bundle_suite_digest: bundle.campaign_digest.clone(),
        bundle_oracle_digest: bundle.oracle_digest.clone(),
        expected_baseline_nonce: baseline_nonce,
        expected_grokptah_nonce: grokptah_nonce,
        expected_implementation_digest: digest(&identity_bytes),
    };
    state.report.verdict = contract_verdict(&state);
    if state.report.verdict != ReviewVerdict::ContractPassed {
        bail!("fake review contract did not pass");
    }
    let completion = if let Some(options) = options {
        Some(seal_fake(options, bundle, &mut state)?)
    } else {
        None
    };
    drop(layout);
    Ok((state, completion))
}

struct InFlightRequest;

struct LoopbackSession {
    memory: BTreeMap<String, String>,
}

struct FakeReviewLoopback {
    workspace: PathBuf,
    workspace_canon: PathBuf,
    canary_path: PathBuf,
    canary_body: String,
    canary_logical_name: String,
    route: String,
    original_route: String,
    frozen: bool,
    in_flight: Option<InFlightRequest>,
    quota_remaining: u32,
    quota_window: u32,
    captures: u32,
    durable_admissions: HashSet<String>,
    sessions: HashMap<String, LoopbackSession>,
    family_sessions: HashMap<String, String>,
    mutator_callback_count: u32,
    publish_callback_count: u32,
    observed_requests: u32,
    observed_tokens: u64,
    restart_count: u32,
    implicit_resend_count: u32,
    duplicate_finding_after_restart: u32,
    route_drift_events: u32,
    in_flight_frozen: u32,
    admissions_blocked_on_drift: u32,
    explicit_requalifications: u32,
    quota_one_under_admitted: u32,
    quota_exhausted_blocked: u32,
    quota_window_advances: u32,
    mutator_denials: u32,
    publish_denials: u32,
    canary_request_hits: u32,
    canary_address_denials: u32,
}

impl FakeReviewLoopback {
    fn new(
        workspace: &Path,
        canary_path: PathBuf,
        canary_body: String,
        canary_logical_name: String,
        route: String,
        quota_window: u32,
    ) -> Result<Self> {
        Ok(Self {
            workspace: workspace.to_path_buf(),
            workspace_canon: dunce::canonicalize(workspace).context("canonicalize workspace")?,
            canary_path,
            canary_body,
            canary_logical_name,
            route: route.clone(),
            original_route: route,
            frozen: false,
            in_flight: None,
            quota_remaining: quota_window,
            quota_window,
            captures: 0,
            durable_admissions: HashSet::new(),
            sessions: HashMap::new(),
            family_sessions: HashMap::new(),
            mutator_callback_count: 0,
            publish_callback_count: 0,
            observed_requests: 0,
            observed_tokens: 0,
            restart_count: 0,
            implicit_resend_count: 0,
            duplicate_finding_after_restart: 0,
            route_drift_events: 0,
            in_flight_frozen: 0,
            admissions_blocked_on_drift: 0,
            explicit_requalifications: 0,
            quota_one_under_admitted: 0,
            quota_exhausted_blocked: 0,
            quota_window_advances: 0,
            mutator_denials: 0,
            publish_denials: 0,
            canary_request_hits: 0,
            canary_address_denials: 0,
        })
    }

    fn run_arm(
        &mut self,
        arm: ArmId,
        family: &RuntimeFamily,
        case: &RuntimeCase,
        script: &ArmScript,
        campaign: &ReviewCampaign,
        canary: &str,
    ) -> Result<(Vec<ScoredFinding>, ArmCost, u32)> {
        for request in &script.requests {
            self.admit_scripted(arm, family, case, request, campaign)?;
        }
        realize_findings(case, script, canary)
    }

    fn session_for(&mut self, arm: ArmId, family_id: &str, case_id: &str) -> String {
        match arm {
            ArmId::Baseline => {
                let id = format!("baseline-{case_id}");
                self.sessions
                    .entry(id.clone())
                    .or_insert_with(|| LoopbackSession {
                        memory: BTreeMap::new(),
                    });
                id
            }
            ArmId::Grokptah => {
                if let Some(id) = self.family_sessions.get(family_id) {
                    return id.clone();
                }
                let id = format!("grokptah-{family_id}");
                self.sessions.insert(
                    id.clone(),
                    LoopbackSession {
                        memory: BTreeMap::new(),
                    },
                );
                self.family_sessions
                    .insert(family_id.to_owned(), id.clone());
                id
            }
        }
    }

    fn scan_body(&mut self, bytes: &[u8]) {
        if body_contains_canary(bytes, &self.canary_body) {
            self.canary_request_hits = self.canary_request_hits.saturating_add(1);
        }
    }

    fn admit_scripted(
        &mut self,
        arm: ArmId,
        family: &RuntimeFamily,
        case: &RuntimeCase,
        request: &crate::review_manifest::ScriptedRequest,
        campaign: &ReviewCampaign,
    ) -> Result<()> {
        if request.prompt_bytes > campaign.prompt_cap_bytes
            || request.completion_bytes > campaign.response_cap_bytes
        {
            bail!("scripted request exceeds prompt or response cap");
        }
        if request.authoritative_tokens == 0 {
            bail!("scripted request is missing authoritative usage");
        }
        let session_id = self.session_for(arm, &family.id, &case.id);
        self.scan_body(&synthetic_fill(request.prompt_bytes));
        self.scan_body(&synthetic_fill(request.completion_bytes));
        if self.canary_request_hits != 0 {
            bail!("canary payload appeared in a transport body");
        }
        if self.frozen {
            self.admissions_blocked_on_drift = self.admissions_blocked_on_drift.saturating_add(1);
            bail!("admission blocked on route drift");
        }
        if self.quota_remaining == 0 {
            self.quota_exhausted_blocked = self.quota_exhausted_blocked.saturating_add(1);
            bail!("quota exhausted");
        }
        for tool in &request.tools {
            if tool.forbidden_for_review() {
                bail!("scripted request used a forbidden tool");
            }
            if matches!(arm, ArmId::Baseline) {
                bail!("baseline request used a tool");
            }
            self.dispatch_allowed_tool(*tool, family, case)?;
        }
        self.quota_remaining = self.quota_remaining.saturating_sub(1);
        self.observed_requests = self.observed_requests.saturating_add(1);
        self.observed_tokens = self
            .observed_tokens
            .checked_add(request.authoritative_tokens)
            .context("token overflow")?;
        self.captures = self.captures.saturating_add(1);
        self.durable_admissions
            .insert(format!("{session_id}:{}:{}", case.id, request.ordinal));
        let _ = session_id;
        Ok(())
    }

    fn dispatch_allowed_tool(
        &mut self,
        tool: ReviewTool,
        family: &RuntimeFamily,
        case: &RuntimeCase,
    ) -> Result<()> {
        match tool {
            ReviewTool::ManifestRead => {
                let file = case.files.first().context("case missing synthetic file")?;
                self.read_workspace_file(&family.id, &file.logical_name)?;
            }
            ReviewTool::Search => {
                for item in &family.cases {
                    for file in &item.files {
                        self.read_workspace_file(&family.id, &file.logical_name)?;
                    }
                }
            }
            ReviewTool::ScopedMemory => {
                let session_id = self
                    .family_sessions
                    .get(&family.id)
                    .cloned()
                    .context("scoped memory without a family session")?;
                let session = self
                    .sessions
                    .get_mut(&session_id)
                    .context("missing family session")?;
                session
                    .memory
                    .insert(case.id.clone(), digest(case.id.as_bytes()));
            }
            other if other.forbidden_for_review() => {
                bail!("forbidden tool reached the allowlisted dispatcher");
            }
            _ => bail!("unsupported review tool"),
        }
        Ok(())
    }

    fn read_workspace_file(&mut self, family_id: &str, logical_name: &str) -> Result<()> {
        if logical_name == self.canary_logical_name || logical_name.contains("..") {
            self.canary_address_denials = self.canary_address_denials.saturating_add(1);
            bail!("workspace read addressed a denied path");
        }
        let relative = format!("{family_id}/{logical_name}.rs");
        let path = self.workspace.join(&relative);
        let canon = dunce::canonicalize(&path).context("canonicalize workspace read")?;
        if !canon.starts_with(&self.workspace_canon) {
            self.canary_address_denials = self.canary_address_denials.saturating_add(1);
            bail!("workspace read escaped the allowlisted root");
        }
        let bytes = std::fs::read(&canon).context("read workspace file")?;
        if body_contains_canary(&bytes, &self.canary_body) {
            bail!("canary payload present in a workspace file");
        }
        self.scan_body(&bytes);
        Ok(())
    }

    fn begin_in_flight(&mut self) -> Result<()> {
        if self.frozen {
            self.admissions_blocked_on_drift = self.admissions_blocked_on_drift.saturating_add(1);
            bail!("cannot start an in-flight request while frozen");
        }
        self.scan_body(&synthetic_fill(32));
        self.in_flight = Some(InFlightRequest);
        Ok(())
    }

    fn restart_drop_inflight(&mut self) -> Result<()> {
        let before = self.captures;
        let durable = self.durable_admissions.len();
        self.in_flight = None;
        self.restart_count = self.restart_count.saturating_add(1);
        if self.captures != before || self.durable_admissions.len() != durable {
            self.implicit_resend_count = self.implicit_resend_count.saturating_add(1);
            bail!("restart resent a durable admission");
        }
        Ok(())
    }

    fn observe_route_drift(&mut self) -> Result<()> {
        if self.in_flight.is_none() {
            bail!("route drift requires an in-flight request");
        }
        self.frozen = true;
        self.route_drift_events = self.route_drift_events.saturating_add(1);
        self.in_flight_frozen = self.in_flight_frozen.saturating_add(1);
        self.route = digest(b"drifted-unqualified-route");
        Ok(())
    }

    fn admit_blocked_on_drift(&mut self) -> Result<()> {
        if !self.frozen {
            bail!("route was not frozen");
        }
        self.scan_body(&synthetic_fill(32));
        self.admissions_blocked_on_drift = self.admissions_blocked_on_drift.saturating_add(1);
        Ok(())
    }

    fn requalify(&mut self) -> Result<()> {
        if !self.frozen {
            bail!("requalify without a frozen route");
        }
        self.frozen = false;
        self.in_flight = None;
        self.route = self.original_route.clone();
        self.explicit_requalifications = self.explicit_requalifications.saturating_add(1);
        Ok(())
    }

    fn admit_observed(&mut self, tokens: u64) -> Result<()> {
        self.scan_body(&synthetic_fill(32));
        if self.frozen {
            self.admissions_blocked_on_drift = self.admissions_blocked_on_drift.saturating_add(1);
            bail!("admission blocked on route drift");
        }
        if self.quota_remaining == 0 {
            self.quota_exhausted_blocked = self.quota_exhausted_blocked.saturating_add(1);
            bail!("quota exhausted");
        }
        self.quota_remaining = self.quota_remaining.saturating_sub(1);
        self.observed_requests = self.observed_requests.saturating_add(1);
        self.observed_tokens = self
            .observed_tokens
            .checked_add(tokens)
            .context("token overflow")?;
        self.captures = self.captures.saturating_add(1);
        self.durable_admissions
            .insert(format!("adv-{}", self.captures));
        Ok(())
    }

    fn quota_one_under(&mut self) -> Result<()> {
        self.quota_remaining = 1;
        self.admit_observed(ADVERSARIAL_REQUEST_TOKENS)?;
        self.quota_one_under_admitted = self.quota_one_under_admitted.saturating_add(1);
        Ok(())
    }

    fn quota_exhausted(&mut self) -> Result<()> {
        self.quota_remaining = 0;
        match self.admit_observed(ADVERSARIAL_REQUEST_TOKENS) {
            Err(_) => Ok(()),
            Ok(()) => bail!("exhausted quota admitted a request"),
        }
    }

    fn quota_window_advance(&mut self) -> Result<()> {
        self.quota_remaining = 0;
        self.quota_remaining = self.quota_window;
        self.quota_window_advances = self.quota_window_advances.saturating_add(1);
        self.admit_observed(ADVERSARIAL_REQUEST_TOKENS)?;
        Ok(())
    }

    fn dispatch_malicious(&mut self, call: &MaliciousCall) -> Result<()> {
        self.scan_body(format!("tool:{}", call.wire_name).as_bytes());
        if !call.tool.forbidden_for_review() {
            bail!("malicious call is not a denied tool");
        }
        if self.mutator_callback_count != 0 || self.publish_callback_count != 0 {
            bail!("mutator callback already recorded");
        }
        match call.tool {
            ReviewTool::Publish => {
                self.publish_denials = self.publish_denials.saturating_add(1);
            }
            _ => {
                self.mutator_denials = self.mutator_denials.saturating_add(1);
            }
        }
        Ok(())
    }

    fn probe_canary(&mut self, logical_name: &str) -> Result<()> {
        if logical_name != self.canary_logical_name {
            bail!("canary probe logical name mismatch");
        }
        self.scan_body(format!("address:{logical_name}").as_bytes());
        if body_contains_canary(logical_name.as_bytes(), &self.canary_body) {
            bail!("canary probe included the secret");
        }
        self.canary_address_denials = self.canary_address_denials.saturating_add(1);
        let escape = self.workspace.join("../inaccessible/canary.dat");
        match dunce::canonicalize(&escape) {
            Ok(canon) if !canon.starts_with(&self.workspace_canon) => {
                self.canary_address_denials = self.canary_address_denials.saturating_add(1);
            }
            Ok(_) => bail!("canary escape resolved inside the workspace"),
            Err(_) => {
                self.canary_address_denials = self.canary_address_denials.saturating_add(1);
            }
        }
        if !self.canary_path.is_file() {
            bail!("inaccessible canary was not materialized");
        }
        Ok(())
    }
}

fn seal_fake(
    options: &ReviewOptions,
    bundle: &ReviewBundle,
    state: &mut EvaluableState,
) -> Result<ReviewCompletion> {
    let output = SafeOutputRoot::open(
        &options.output_root,
        &options.repository_root,
        None,
        options.artifact_budget_bytes,
    )?;
    let campaign_id = format!("review-fake-{}", &Uuid::new_v4().simple().to_string()[..12]);
    let artifacts = output.create_campaign(&campaign_id)?;
    let campaign_digest =
        artifacts.write_final("contract/campaign.json", &bundle.campaign_bytes)?;
    let fingerprint = fingerprint_from_bundle(bundle)?;
    let fingerprint_bytes = serde_json::to_vec_pretty(&fingerprint)?;
    scan_value_for_forbidden_data(&serde_json::to_value(&fingerprint)?)
        .map_err(|_| anyhow!("review fingerprint failed forbidden-data scanning"))?;
    let fingerprint_digest =
        artifacts.write_final("contract/fingerprint.json", &fingerprint_bytes)?;
    let identity_bytes = implementation_bytes(bundle)?;
    let identity_digest = artifacts.write_final("contract/implementation.json", &identity_bytes)?;
    state.report.implementation_digest = identity_digest.sha256.clone();
    state.report.artifacts = vec![
        public_ref(&campaign_digest, ArtifactRole::SuiteManifest),
        public_ref(&fingerprint_digest, ArtifactRole::DigestFingerprint),
        public_ref(&identity_digest, ArtifactRole::ImplementationIdentity),
    ];
    state.report.bounds_actual.artifact_bytes = sum_artifact_bytes(&state.report.artifacts)?;
    state.report.completeness.artifacts_consumed = true;
    state.report.verdict = contract_verdict(state);
    let report_bytes = state.report.validate_structure()?;
    if report_bytes.len() > MAX_REVIEW_REPORT_BYTES {
        bail!("review report exceeds public artifact bound");
    }
    let sealed = artifacts.write_final("report.json", &report_bytes)?;
    artifacts.mark_complete(&sealed)?;
    Ok(completion_from_sealed(&state.report, &sealed.sha256))
}

fn base_report(bundle: &ReviewBundle, mode: ReviewMode) -> ReviewReport {
    let zero = crate::review_report::ArmPublicMetrics {
        true_positives: 0,
        false_positives: 0,
        false_negatives: 0,
        precision: 0.0,
        recall: 0.0,
        f1: 0.0,
        weighted_precision: 0.0,
        weighted_recall: 0.0,
        weighted_f1: 0.0,
        high_critical_recall: 0.0,
        usefulness: 0.0,
        brier: 0.0,
        ece: 0.0,
        completeness_brier: 0.0,
        weighted_utility: 0.0,
    };
    ReviewReport {
        schema: REVIEW_REPORT_SCHEMA.into(),
        campaign_id: bundle.campaign.campaign_id.clone(),
        suite_id: bundle.campaign.suite_id.clone(),
        suite_digest: bundle.campaign_digest.clone(),
        scorer_digest: scorer_digest(),
        runner_digest: runner_digest(),
        corpus_digest: bundle.corpus_digest.clone(),
        oracle_digest: bundle.oracle_digest.clone(),
        fake_provider_digest: bundle.fake_provider_digest.clone(),
        implementation_digest: digest(b"unset-implementation"),
        mode,
        runtime_kind: match mode {
            ReviewMode::Fake => ReviewRuntimeKind::FakeLoopbackTransport,
            ReviewMode::Live => ReviewRuntimeKind::LiveEnterpriseUnimplemented,
        },
        binding: OpaqueBinding {
            pair_nonce: digest(b"unset"),
            baseline_arm_nonce: digest(b"unset-baseline"),
            grokptah_arm_nonce: digest(b"unset-grokptah"),
            route_fingerprint: digest(b"unset-route"),
            deployment_fingerprint: digest(b"unset-deploy"),
            credential_fingerprint: digest(b"unset-cred"),
            model_fingerprint: bundle.fake_provider.binding.model_fingerprint.clone(),
            effort: bundle.fake_provider.binding.effort.clone(),
            decoding: bundle.fake_provider.binding.decoding.clone(),
            prompt_cap_bytes: bundle.campaign.prompt_cap_bytes,
            response_cap_bytes: bundle.campaign.response_cap_bytes,
            corpus_digest: bundle.corpus_digest.clone(),
            attestation_present: false,
            attestation_valid: false,
            modest_tier_attested: false,
            premium_fallback_attested_absent: false,
            egress_attestation_present: false,
        },
        bounds_configured: bundle.campaign.bounds.clone(),
        bounds_actual: ActualBounds {
            provider_requests: 0,
            authoritative_tokens: 0,
            duration_millis: 0,
            continuations: 0,
            artifact_bytes: 0,
            baseline_max_requests_per_case: 0,
            grokptah_max_requests_per_case: 0,
            baseline_max_tokens_per_case: 0,
            grokptah_max_tokens_per_case: 0,
        },
        metrics: crate::review_report::PublicMetrics {
            baseline: zero.clone(),
            grokptah: zero,
        },
        deltas: crate::review_report::PublicDeltas {
            precision: 0.0,
            weighted_recall: 0.0,
            weighted_utility: 0.0,
            recall: 0.0,
            token_ratio: 0.0,
            request_ratio: 0.0,
            wall_ratio: 0.0,
            utility_gain_per_10k_tokens: 0.0,
        },
        cis: crate::review_report::PublicCis {
            weighted_utility_lift_lower: 0.0,
            recall_lift_lower: 0.0,
            efficiency_superiority_claimable: false,
        },
        wins: crate::review_report::WinCard {
            family_wins: 0,
            family_count: EXPECTED_FAMILY_COUNT as u32,
            worst_family_delta: 0.0,
        },
        family_utility: Default::default(),
        cardinalities: Cardinalities {
            restart_count: 0,
            implicit_resend_count: 0,
            duplicate_finding_after_restart: 0,
            route_drift_events: 0,
            in_flight_frozen: 0,
            admissions_blocked_on_drift: 0,
            explicit_requalifications: 0,
            quota_one_under_admitted: 0,
            quota_exhausted_blocked: 0,
            quota_window_advances: 0,
            mutator_denials: 0,
            publish_denials: 0,
            canary_request_hits: 0,
            canary_evidence_hits: 0,
            canary_address_denials: 0,
            cases_scored: 0,
            families_scored: 0,
            live_replicates_configured: bundle.campaign.live_replicate_count,
            live_replicates_executed: 0,
        },
        workspace: WorkspaceHashes {
            pre_merkle_root: digest(b"empty"),
            post_merkle_root: digest(b"empty"),
            pre_git_head: digest(b"empty"),
            post_git_head: digest(b"empty"),
            pre_git_refs: digest(b"empty"),
            post_git_refs: digest(b"empty"),
            pre_publication_count: 0,
            post_publication_count: 0,
        },
        completeness: Completeness {
            provider_observation_complete: false,
            fake_transport_observation_complete: false,
            authoritative_usage_complete: false,
            egress_attestation_complete: false,
            deployment_attestation_complete: false,
            actions_consumed: false,
            oracles_consumed: false,
            cases_consumed: false,
            artifacts_consumed: false,
            bounds_consumed: false,
        },
        forbidden_scan_passed: false,
        quality_claim_eligible: false,
        verdict: ReviewVerdict::Indeterminate,
        quality_claim: QualityClaim::NotClaimed,
        live_indeterminate_reasons: Vec::new(),
        artifacts: Vec::new(),
    }
}

fn realize_findings(
    case: &RuntimeCase,
    script: &ArmScript,
    canary: &str,
) -> Result<(Vec<ScoredFinding>, ArmCost, u32)> {
    let mut evidence_hits = 0u32;
    let mut tokens = 0u64;
    for request in &script.requests {
        tokens = tokens
            .checked_add(request.authoritative_tokens)
            .context("script token overflow")?;
    }
    let mut findings = Vec::new();
    for finding in &script.findings {
        let file = case
            .files
            .iter()
            .find(|file| file.logical_name == finding.logical_file)
            .context("scripted finding file is not in the runtime case")?;
        if finding.causal_atom.contains(canary) || finding.logical_file.contains(canary) {
            evidence_hits += 1;
        }
        findings.push(ScoredFinding {
            case_id: case.id.clone(),
            family_id: case.family_id.clone(),
            opaque_file_id: file.opaque_file_id.clone(),
            opaque_symbol_id: opaque_id(
                "sym",
                &[&case.family_id, &case.id, &finding.logical_symbol],
            ),
            region: crate::review_manifest::LineRegion {
                start_line: finding.start_line,
                end_line: finding.end_line,
            },
            category: finding.category,
            causal_atom: finding.causal_atom.clone(),
            severity: finding.severity,
            confidence_millis: finding.confidence_millis,
            usefulness: finding.usefulness,
        });
    }
    let cost = ArmCost {
        requests: u32::try_from(script.requests.len()).context("request count")?,
        authoritative_tokens: tokens,
        wall_millis: u64::from(u32::try_from(script.requests.len()).unwrap_or(1)) * 8,
    };
    Ok((findings, cost, evidence_hits))
}

fn materialize_workspace(workspace: &Path, runtime: &RuntimeCorpus) -> Result<()> {
    for family in &runtime.families {
        let dir = workspace.join(&family.id);
        std::fs::create_dir(&dir).context("create family workspace")?;
        for case in &family.cases {
            for file in &case.files {
                std::fs::write(
                    dir.join(format!("{}.rs", file.logical_name)),
                    file.body.as_bytes(),
                )
                .context("write synthetic review file")?;
            }
        }
    }
    Ok(())
}

fn implementation_identity(bundle: &ReviewBundle) -> ReviewImplementationIdentity {
    ReviewImplementationIdentity {
        schema: REVIEW_IMPLEMENTATION_SCHEMA.into(),
        scorer_source_sha256: scorer_digest(),
        runner_source_sha256: runner_digest(),
        manifest_source_sha256: manifest_source_digest(),
        report_source_sha256: report_source_digest(),
        scorer_contract: SCORER_IDENTITY.to_owned(),
        runner_contract: RUNNER_IDENTITY.to_owned(),
        bridge_version: BRIDGE_VERSION.to_owned(),
        bundled_campaign_sha256: digest(BUNDLED_REVIEW_CAMPAIGN),
        bundled_corpus_sha256: digest(BUNDLED_REVIEW_CORPUS),
        bundled_fake_provider_sha256: digest(BUNDLED_REVIEW_FAKE_PROVIDER),
        loaded_campaign_sha256: bundle.campaign_digest.clone(),
        loaded_corpus_sha256: bundle.corpus_digest.clone(),
        loaded_fake_provider_sha256: bundle.fake_provider_digest.clone(),
        loaded_oracle_sha256: bundle.oracle_digest.clone(),
    }
}

fn implementation_bytes(bundle: &ReviewBundle) -> Result<Vec<u8>> {
    let identity = implementation_identity(bundle);
    scan_value_for_forbidden_data(&serde_json::to_value(&identity)?)
        .map_err(|_| anyhow!("implementation identity failed forbidden-data scanning"))?;
    serde_json::to_vec_pretty(&identity).context("serialize implementation identity")
}

fn sum_artifact_bytes(artifacts: &[PublicArtifactRef]) -> Result<u64> {
    let mut total = 0u64;
    for artifact in artifacts {
        total = total
            .checked_add(artifact.bytes)
            .ok_or_else(|| anyhow!("artifact byte overflow"))?;
    }
    Ok(total)
}

fn synthetic_fill(bytes: u64) -> Vec<u8> {
    vec![b'x'; usize::try_from(bytes).unwrap_or(0)]
}

fn body_contains_canary(bytes: &[u8], canary: &str) -> bool {
    if canary.is_empty() {
        return false;
    }
    bytes
        .windows(canary.len())
        .any(|window| window == canary.as_bytes())
}

fn assert_tree_excludes_canary(root: &Path, canary: &str) -> Result<()> {
    fn walk(directory: &Path, canary: &str) -> Result<()> {
        let mut children = Vec::new();
        for child in std::fs::read_dir(directory).context("read workspace tree for canary")? {
            children.push(child.context("read workspace entry")?);
        }
        for child in children {
            let path = child.path();
            let metadata = std::fs::symlink_metadata(&path).context("stat workspace entry")?;
            if metadata.file_type().is_symlink() {
                bail!("workspace tree contains a symbolic link");
            }
            if child.file_name() == ".git" {
                continue;
            }
            if metadata.is_dir() {
                walk(&path, canary)?;
            } else if metadata.is_file() {
                if child.file_name() == "canary.dat" {
                    bail!("canary file is present in the review workspace");
                }
                let bytes = std::fs::read(&path).context("read workspace file")?;
                if body_contains_canary(&bytes, canary) {
                    bail!("canary payload is present in the review workspace");
                }
            } else {
                bail!("workspace tree contains a non-regular entry");
            }
        }
        Ok(())
    }
    walk(root, canary)
}

fn completion_from_sealed(report: &ReviewReport, report_sha256: &str) -> ReviewCompletion {
    ReviewCompletion {
        campaign_id: report.campaign_id.clone(),
        verdict: report.verdict,
        quality_claim_eligible: report.quality_claim_eligible,
        fake_cannot_prove_quality: report.quality_claim == QualityClaim::FakeCannotProveQuality,
        report_sha256: report_sha256.to_owned(),
        notice: report.quality_claim,
    }
}

fn init_git(workspace: &Path) -> Result<()> {
    run_git(workspace, &["init", "-q", "-b", "main"])?;
    run_git(workspace, &["add", "-A"])?;
    run_git(
        workspace,
        &[
            "-c",
            "user.name=review",
            "-c",
            "user.email=review@git.invalid",
            "commit",
            "-q",
            "--no-gpg-sign",
            "--no-verify",
            "-m",
            "synthetic",
        ],
    )?;
    Ok(())
}

fn run_git(workspace: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workspace)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_AUTHOR_NAME", "review")
        .env("GIT_AUTHOR_EMAIL", "review@git.invalid")
        .env("GIT_COMMITTER_NAME", "review")
        .env("GIT_COMMITTER_EMAIL", "review@git.invalid")
        .output()
        .context("invoke git")?;
    if !output.status.success() {
        bail!("git command failed");
    }
    Ok(())
}

fn hashes(
    pre_merkle: &str,
    post_merkle: &str,
    pre_git: &GitRefSnapshot,
    post_git: &GitRefSnapshot,
) -> WorkspaceHashes {
    WorkspaceHashes {
        pre_merkle_root: pre_merkle.to_owned(),
        post_merkle_root: post_merkle.to_owned(),
        pre_git_head: pre_git.head_digest.clone(),
        post_git_head: post_git.head_digest.clone(),
        pre_git_refs: pre_git.refs_digest.clone(),
        post_git_refs: post_git.refs_digest.clone(),
        pre_publication_count: pre_git.remote_publication_count,
        post_publication_count: post_git.remote_publication_count,
    }
}

fn fingerprint_from_bundle(bundle: &ReviewBundle) -> Result<ReviewFingerprint> {
    let identity_bytes = implementation_bytes(bundle)?;
    Ok(ReviewFingerprint {
        schema: REVIEW_FINGERPRINT_SCHEMA.into(),
        campaign_id: bundle.campaign.campaign_id.clone(),
        suite_digest: bundle.campaign_digest.clone(),
        corpus_digest: bundle.corpus_digest.clone(),
        oracle_digest: bundle.oracle_digest.clone(),
        scorer_digest: scorer_digest(),
        runner_digest: runner_digest(),
        fake_provider_digest: bundle.fake_provider_digest.clone(),
        implementation_digest: digest(&identity_bytes),
        scorer_source_sha256: scorer_digest(),
        runner_source_sha256: runner_digest(),
        bundled_campaign_sha256: digest(BUNDLED_REVIEW_CAMPAIGN),
        bundled_corpus_sha256: digest(BUNDLED_REVIEW_CORPUS),
        bundled_fake_provider_sha256: digest(BUNDLED_REVIEW_FAKE_PROVIDER),
        case_ids: bundle.campaign.case_ids(),
        family_ids: bundle
            .campaign
            .families
            .iter()
            .map(|family| family.id.clone())
            .collect(),
        action_count: bundle.campaign.actions.len() as u32,
        oracle_count: bundle.campaign.oracles.len() as u32,
        artifact_count: bundle.campaign.artifacts.len() as u32,
    })
}

fn public_ref(digest: &crate::artifact::ArtifactDigest, role: ArtifactRole) -> PublicArtifactRef {
    PublicArtifactRef {
        relative_path: digest.relative_path.clone(),
        sha256: digest.sha256.clone(),
        bytes: digest.bytes,
        role,
    }
}

fn add_cost(total: &mut ArmCost, add: ArmCost) -> Result<()> {
    total.requests = total
        .requests
        .checked_add(add.requests)
        .context("requests")?;
    total.authoritative_tokens = total
        .authoritative_tokens
        .checked_add(add.authoritative_tokens)
        .context("tokens")?;
    total.wall_millis = total
        .wall_millis
        .checked_add(add.wall_millis)
        .context("wall")?;
    Ok(())
}

fn tick<T: Copy + Ord + std::fmt::Debug>(remaining: &mut BTreeSet<T>, item: T) -> Result<()> {
    if !remaining.remove(&item) {
        bail!("declared item was missing or already consumed: {item:?}");
    }
    Ok(())
}

fn ambient_override_present() -> bool {
    LIVE_ROUTE_OVERRIDE_ENVS
        .iter()
        .any(|name| std::env::var_os(name).is_some())
}

pub fn validate_review_output_location(output: &Path, repository: &Path) -> Result<()> {
    let repository = dunce::canonicalize(repository).context("canonicalize repository root")?;
    if output.starts_with(&repository)
        && !output.starts_with(repository.join(REVIEW_OUTPUT_RELATIVE_PATH))
        && !output.starts_with(repository.join(DEFAULT_OUTPUT_RELATIVE_PATH))
    {
        bail!("output_path_not_in_precise_ignored_root");
    }
    Ok(())
}

fn validate_review_options(options: &ReviewOptions) -> Result<()> {
    if !options.repository_root.is_absolute()
        || !options.campaign_path.is_absolute()
        || !options.output_root.is_absolute()
    {
        bail!("certification_paths_must_be_absolute");
    }
    if options.artifact_budget_bytes == 0 || options.artifact_budget_bytes > 128 * 1024 * 1024 {
        bail!("campaign_bounds_invalid");
    }
    validate_review_output_location(&options.output_root, &options.repository_root)?;
    if options.mode == ReviewMode::Live && ambient_override_present() {
        bail!("live_ambient_route_or_credential_override_present");
    }
    Ok(())
}

pub fn stderr_progress(mode: ReviewMode, phase: &str) {
    match mode {
        ReviewMode::Fake => eprintln!("grokptah-cert: {phase} fake_cannot_prove_quality"),
        ReviewMode::Live => {
            eprintln!("grokptah-cert: {phase} live_enterprise_gateway_review_indeterminate")
        }
    }
}

pub fn latest_review_campaign_dir(output_root: &Path) -> Result<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(output_root).context("read review output root")? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("review-fake-") || name.starts_with("review-live-")) {
            continue;
        }
        if let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) {
            if newest.as_ref().is_none_or(|(time, _)| modified >= *time) {
                newest = Some((modified, path));
            }
        }
    }
    newest
        .map(|(_, path)| path)
        .ok_or_else(|| anyhow::anyhow!("review campaign directory not found"))
}
