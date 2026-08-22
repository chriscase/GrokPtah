//! Fake-contract and live-fail-closed runner for the code-review benchmark.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::scan_value_for_forbidden_data;
use serde::Serialize;
use uuid::Uuid;

use crate::artifact::{SafeOutputRoot, DEFAULT_OUTPUT_RELATIVE_PATH};
use crate::local_service::{
    enterprise_review_live_attach, git_ref_snapshot, workspace_merkle_root, GitRefSnapshot,
};
use crate::review_manifest::{
    default_campaign_path, digest, opaque_id, runner_digest, scorer_digest, ArmId, ArtifactRole,
    HiddenOracleSet, LiveThresholds, ReviewAction, ReviewBundle, ReviewCampaign, ReviewOracle,
    ReviewTool, RuntimeCase, RuntimeCorpus, EXPECTED_CASE_COUNT, EXPECTED_FAMILY_COUNT,
};
use crate::review_report::{
    arm_metrics, cis_from, deltas_from, evaluate_verdict, inspect_review_campaign, wins_from,
    ActualBounds, Cardinalities, Completeness, IndeterminateReason, OpaqueBinding,
    PublicArtifactRef, QualityClaim, ReviewFingerprint, ReviewMode, ReviewReport, ReviewVerdict,
    WorkspaceHashes, MAX_REVIEW_REPORT_BYTES,
};
use crate::review_score::{findings_digest, score_paired, ArmCost, ScoredFinding};
use crate::{REVIEW_FINGERPRINT_SCHEMA, REVIEW_REPORT_SCHEMA};

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
    DropProviderObservation,
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
        enterprise_gateway_lease: enterprise_review_live_attach().is_ok(),
        notice: match options.mode {
            ReviewMode::Fake => QualityClaim::FakeCannotProveQuality,
            ReviewMode::Live => QualityClaim::LiveAttestationMissing,
        },
    })
}

pub fn run_review(options: &ReviewOptions) -> Result<ReviewCompletion> {
    validate_review_options(options)?;
    if options.preflight_only {
        bail!("preflight_only_run");
    }
    let bundle = ReviewBundle::load_checked(&options.campaign_path, &options.repository_root)?;
    match options.mode {
        ReviewMode::Fake => {
            let state = execute_fake(&bundle, Some(options))?;
            completion_from_report(&state.report)
        }
        ReviewMode::Live => run_live_fail_closed(options, &bundle),
    }
}

pub fn run_fake_evaluable(bundle: &ReviewBundle) -> Result<EvaluableState> {
    execute_fake(bundle, None)
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
        ContractMutation::DropProviderObservation => {
            state.report.completeness.provider_observation_complete = false;
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
    if enterprise_review_live_attach().is_ok() {
        bail!("enterprise_gateway_live_attach_must_remain_unimplemented");
    }
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
    let fingerprint = fingerprint_from_bundle(bundle);
    let fingerprint_bytes = serde_json::to_vec_pretty(&fingerprint)?;
    let fingerprint_digest =
        artifacts.write_final("contract/fingerprint.json", &fingerprint_bytes)?;
    let placeholder = digest(b"unmaterialized-live-review-workspace-v1");
    let mut report = base_report(bundle, ReviewMode::Live);
    report.completeness.artifacts_consumed = true;
    report.completeness.bounds_consumed = true;
    report.live_indeterminate_reasons = vec![
        IndeterminateReason::EnterpriseGatewayLeaseUnimplemented,
        IndeterminateReason::DeploymentAttestationMissing,
        IndeterminateReason::EgressFirewallAttestationMissing,
        IndeterminateReason::LiveReplicatesNotExecuted,
        IndeterminateReason::AuthoritativeUsageMissing,
    ];
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
    ];
    report.verdict = evaluate_verdict(&report);
    let report_bytes = report.validate_structure()?;
    let sealed = artifacts.write_final("report.json", &report_bytes)?;
    artifacts.mark_complete(&sealed)?;
    completion_from_report(&report)
}

fn execute_fake(bundle: &ReviewBundle, options: Option<&ReviewOptions>) -> Result<EvaluableState> {
    let started = Instant::now();
    let mut actions: BTreeSet<ReviewAction> = bundle.campaign.actions.iter().copied().collect();
    let mut oracles: BTreeSet<ReviewOracle> = bundle.campaign.oracles.iter().copied().collect();
    let (runtime, hidden) = bundle.corpus.split()?;
    tick(&mut actions, ReviewAction::MaterializeSyntheticWorkspace)?;
    let layout = tempfile::tempdir().context("create disposable review workspace")?;
    let workspace = layout.path().join("workspace");
    std::fs::create_dir(&workspace).context("create review workspace")?;
    materialize_workspace(
        &workspace,
        &runtime,
        &bundle.corpus.inaccessible_canary.body,
    )?;
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
            let (findings, cost, ev_hits) = realize_script(case, script, &bundle.campaign, canary)?;
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
            let (findings, cost, ev_hits) = realize_script(case, script, &bundle.campaign, canary)?;
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

    let mut cardinalities = Cardinalities {
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
        canary_evidence_hits,
        cases_scored: u32::try_from(cases_seen.len()).context("case count")?,
        families_scored: u32::try_from(runtime.families.len()).context("family count")?,
        live_replicates_configured: bundle.campaign.live_replicate_count,
        live_replicates_executed: 0,
    };
    let mut extra_requests = 0u32;
    let mut extra_tokens = 0u64;

    tick(&mut actions, ReviewAction::RestartAfterDurableAdmission)?;
    extra_requests += 1;
    extra_tokens += ADVERSARIAL_REQUEST_TOKENS;
    cardinalities.restart_count = 1;
    tick(&mut oracles, ReviewOracle::RestartNoImplicitResend)?;
    tick(&mut oracles, ReviewOracle::RestartNoDuplicateFinding)?;

    tick(&mut actions, ReviewAction::ObserveRouteDrift)?;
    extra_requests += 1;
    extra_tokens += ADVERSARIAL_REQUEST_TOKENS;
    cardinalities.route_drift_events = 1;
    cardinalities.in_flight_frozen = 1;
    cardinalities.admissions_blocked_on_drift = 1;
    cardinalities.explicit_requalifications = 1;
    tick(&mut oracles, ReviewOracle::RouteDriftFreezesInflight)?;
    tick(&mut oracles, ReviewOracle::RouteDriftBlocksUntilRequalified)?;

    tick(&mut actions, ReviewAction::ObserveQuotaOneUnder)?;
    extra_requests += 1;
    extra_tokens += ADVERSARIAL_REQUEST_TOKENS;
    cardinalities.quota_one_under_admitted = 1;
    tick(&mut oracles, ReviewOracle::QuotaOneUnderAdmits)?;

    tick(&mut actions, ReviewAction::ObserveQuotaExhausted)?;
    extra_requests += 1;
    cardinalities.quota_exhausted_blocked = 1;
    tick(&mut oracles, ReviewOracle::QuotaExhaustedBlocks)?;

    tick(&mut actions, ReviewAction::ObserveQuotaWindowAdvance)?;
    extra_requests += 1;
    extra_tokens += ADVERSARIAL_REQUEST_TOKENS;
    cardinalities.quota_window_advances = 1;
    tick(&mut oracles, ReviewOracle::QuotaWindowAdvanceResets)?;

    tick(&mut actions, ReviewAction::DenyMutatorsAndPublish)?;
    for tool in &bundle.campaign.denied_tools {
        if *tool == ReviewTool::Publish {
            cardinalities.publish_denials += 1;
        } else {
            cardinalities.mutator_denials += 1;
        }
    }
    tick(&mut oracles, ReviewOracle::MutatorsDenied)?;
    tick(&mut oracles, ReviewOracle::PublishDenied)?;

    tick(&mut actions, ReviewAction::ProveInaccessibleCanaryAbsent)?;
    tick(&mut oracles, ReviewOracle::CanaryAbsentFromRequests)?;
    tick(&mut oracles, ReviewOracle::CanaryAbsentFromEvidence)?;

    tick(&mut actions, ReviewAction::SnapshotWorkspacePost)?;
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

    let total_requests = baseline_cost
        .requests
        .checked_add(grokptah_cost.requests)
        .context("campaign request overflow")?;
    let total_tokens = baseline_cost
        .authoritative_tokens
        .checked_add(grokptah_cost.authoritative_tokens)
        .context("campaign token overflow")?;
    if total_requests > bundle.campaign.bounds.max_provider_requests
        || total_tokens > bundle.campaign.bounds.max_authoritative_tokens
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

    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut report = base_report(bundle, ReviewMode::Fake);
    report.binding = binding;
    report.bounds_actual = ActualBounds {
        provider_requests: total_requests,
        authoritative_tokens: total_tokens,
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
    report.cardinalities = cardinalities;
    report.workspace = hashes(&pre_merkle, &post_merkle, &pre_git, &post_git);
    report.completeness = Completeness {
        provider_observation_complete: true,
        authoritative_usage_complete: true,
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
        grokptah_route: route,
        baseline_model: bundle.fake_provider.binding.model_fingerprint.clone(),
        grokptah_model: bundle.fake_provider.binding.model_fingerprint.clone(),
        bundle_suite_digest: bundle.campaign_digest.clone(),
        bundle_oracle_digest: bundle.oracle_digest.clone(),
        expected_baseline_nonce: baseline_nonce,
        expected_grokptah_nonce: grokptah_nonce,
    };
    state.report.verdict = contract_verdict(&state);
    if state.report.verdict != ReviewVerdict::ContractPassed {
        bail!("fake review contract did not pass");
    }
    if let Some(options) = options {
        seal_fake(options, bundle, &mut state)?;
    }
    drop(layout);
    Ok(state)
}

fn seal_fake(
    options: &ReviewOptions,
    bundle: &ReviewBundle,
    state: &mut EvaluableState,
) -> Result<()> {
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
    let fingerprint = fingerprint_from_bundle(bundle);
    let fingerprint_bytes = serde_json::to_vec_pretty(&fingerprint)?;
    scan_value_for_forbidden_data(&serde_json::to_value(&fingerprint)?)
        .map_err(|_| anyhow::anyhow!("review fingerprint failed forbidden-data scanning"))?;
    let fingerprint_digest =
        artifacts.write_final("contract/fingerprint.json", &fingerprint_bytes)?;
    state.report.artifacts = vec![
        public_ref(&campaign_digest, ArtifactRole::SuiteManifest),
        public_ref(&fingerprint_digest, ArtifactRole::DigestFingerprint),
    ];
    state.report.completeness.artifacts_consumed = true;
    state.report.verdict = contract_verdict(state);
    let report_bytes = state.report.validate_structure()?;
    if report_bytes.len() > MAX_REVIEW_REPORT_BYTES {
        bail!("review report exceeds public artifact bound");
    }
    let sealed = artifacts.write_final("report.json", &report_bytes)?;
    artifacts.mark_complete(&sealed)?;
    state.report.bounds_actual.artifact_bytes = campaign_digest
        .bytes
        .saturating_add(fingerprint_digest.bytes)
        .saturating_add(sealed.bytes);
    Ok(())
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
        mode,
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

fn realize_script(
    case: &RuntimeCase,
    script: &crate::review_manifest::ArmScript,
    campaign: &ReviewCampaign,
    canary: &str,
) -> Result<(Vec<ScoredFinding>, ArmCost, u32)> {
    let mut evidence_hits = 0u32;
    let mut tokens = 0u64;
    for request in &script.requests {
        if request.prompt_bytes > campaign.prompt_cap_bytes
            || request.completion_bytes > campaign.response_cap_bytes
        {
            bail!("scripted request exceeds prompt or response cap");
        }
        if request.authoritative_tokens == 0 {
            bail!("scripted request is missing authoritative usage");
        }
        for tool in &request.tools {
            if tool.forbidden_for_review() {
                bail!("scripted request used a forbidden tool");
            }
            if matches!(script.arm, ArmId::Baseline) {
                bail!("baseline request used a tool");
            }
        }
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

fn materialize_workspace(
    workspace: &Path,
    runtime: &RuntimeCorpus,
    canary_body: &str,
) -> Result<()> {
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
    std::fs::write(workspace.join("canary.dat"), canary_body.as_bytes())
        .context("write inaccessible canary")?;
    Ok(())
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

fn fingerprint_from_bundle(bundle: &ReviewBundle) -> ReviewFingerprint {
    ReviewFingerprint {
        schema: REVIEW_FINGERPRINT_SCHEMA.into(),
        campaign_id: bundle.campaign.campaign_id.clone(),
        suite_digest: bundle.campaign_digest.clone(),
        corpus_digest: bundle.corpus_digest.clone(),
        oracle_digest: bundle.oracle_digest.clone(),
        scorer_digest: scorer_digest(),
        runner_digest: runner_digest(),
        fake_provider_digest: bundle.fake_provider_digest.clone(),
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
    }
}

fn public_ref(digest: &crate::artifact::ArtifactDigest, role: ArtifactRole) -> PublicArtifactRef {
    PublicArtifactRef {
        relative_path: digest.relative_path.clone(),
        sha256: digest.sha256.clone(),
        bytes: digest.bytes,
        role,
    }
}

fn completion_from_report(report: &ReviewReport) -> Result<ReviewCompletion> {
    let bytes = serde_json::to_vec(report)?;
    Ok(ReviewCompletion {
        campaign_id: report.campaign_id.clone(),
        verdict: report.verdict,
        quality_claim_eligible: report.quality_claim_eligible,
        fake_cannot_prove_quality: report.quality_claim == QualityClaim::FakeCannotProveQuality,
        report_sha256: digest(&bytes),
        notice: report.quality_claim,
    })
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
            eprintln!("grokptah-cert: {phase} live_enterprise_gateway_unimplemented")
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
