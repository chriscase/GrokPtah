//! Public sealed report schema `grokptah.code-review-benchmark.v1`.
//!
//! Artifacts are numeric and structural only: no source, paths, prompts,
//! completions, endpoints, credentials, or identities.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::scan_value_for_forbidden_data;
use serde::{Deserialize, Serialize};

use crate::artifact::{verify_completed_campaign, ArtifactDigest, ArtifactStage};
use crate::review_manifest::{
    digest, manifest_source_digest, report_source_digest, runner_digest, scorer_digest,
    validate_kebab_id, ArtifactRole, CampaignBounds, ReviewBundle, BUNDLED_REVIEW_CAMPAIGN,
    BUNDLED_REVIEW_CORPUS, BUNDLED_REVIEW_FAKE_PROVIDER, MANIFEST_SOURCE, REPORT_SOURCE,
    RUNNER_SOURCE, SCORER_SOURCE,
};
use crate::review_score::PairedScore;
use crate::{
    REVIEW_CAMPAIGN_SCHEMA, REVIEW_FINGERPRINT_SCHEMA, REVIEW_IMPLEMENTATION_SCHEMA,
    REVIEW_REPORT_SCHEMA,
};

pub const MAX_REVIEW_REPORT_BYTES: usize = 512 * 1024;
pub const MAX_FINGERPRINT_BYTES: usize = 64 * 1024;
pub const MAX_IMPLEMENTATION_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewRuntimeKind {
    FakeLoopbackTransport,
    LiveEnterpriseUnimplemented,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    Fake,
    Live,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewVerdict {
    ContractPassed,
    Failed,
    Indeterminate,
    SafetyFailure,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QualityClaim {
    FakeCannotProveQuality,
    LiveAttestationMissing,
    NotClaimed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IndeterminateReason {
    EnterpriseGatewayLeaseUnimplemented,
    DeploymentAttestationMissing,
    DeploymentAttestationExpired,
    DeploymentAttestationMixed,
    EgressFirewallAttestationMissing,
    AuthoritativeUsageMissing,
    ProviderObservationDropped,
    LiveReplicatesNotExecuted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OpaqueBinding {
    pub pair_nonce: String,
    pub baseline_arm_nonce: String,
    pub grokptah_arm_nonce: String,
    pub route_fingerprint: String,
    pub deployment_fingerprint: String,
    pub credential_fingerprint: String,
    pub model_fingerprint: String,
    pub effort: String,
    pub decoding: String,
    pub prompt_cap_bytes: u64,
    pub response_cap_bytes: u64,
    pub corpus_digest: String,
    pub attestation_present: bool,
    pub attestation_valid: bool,
    pub modest_tier_attested: bool,
    pub premium_fallback_attested_absent: bool,
    pub egress_attestation_present: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActualBounds {
    pub provider_requests: u32,
    pub authoritative_tokens: u64,
    pub duration_millis: u64,
    pub continuations: u32,
    pub artifact_bytes: u64,
    pub baseline_max_requests_per_case: u32,
    pub grokptah_max_requests_per_case: u32,
    pub baseline_max_tokens_per_case: u64,
    pub grokptah_max_tokens_per_case: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ArmPublicMetrics {
    pub true_positives: u32,
    pub false_positives: u32,
    pub false_negatives: u32,
    pub precision: f64,
    pub recall: f64,
    pub f1: f64,
    pub weighted_precision: f64,
    pub weighted_recall: f64,
    pub weighted_f1: f64,
    pub high_critical_recall: f64,
    pub usefulness: f64,
    pub brier: f64,
    pub ece: f64,
    pub completeness_brier: f64,
    pub weighted_utility: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicMetrics {
    pub baseline: ArmPublicMetrics,
    pub grokptah: ArmPublicMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicDeltas {
    pub precision: f64,
    pub weighted_recall: f64,
    pub weighted_utility: f64,
    pub recall: f64,
    pub token_ratio: f64,
    pub request_ratio: f64,
    pub wall_ratio: f64,
    pub utility_gain_per_10k_tokens: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PublicCis {
    pub weighted_utility_lift_lower: f64,
    pub recall_lift_lower: f64,
    pub efficiency_superiority_claimable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WinCard {
    pub family_wins: u32,
    pub family_count: u32,
    pub worst_family_delta: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Cardinalities {
    pub restart_count: u32,
    pub implicit_resend_count: u32,
    pub duplicate_finding_after_restart: u32,
    pub route_drift_events: u32,
    pub in_flight_frozen: u32,
    pub admissions_blocked_on_drift: u32,
    pub explicit_requalifications: u32,
    pub quota_one_under_admitted: u32,
    pub quota_exhausted_blocked: u32,
    pub quota_window_advances: u32,
    pub mutator_denials: u32,
    pub publish_denials: u32,
    pub canary_request_hits: u32,
    pub canary_evidence_hits: u32,
    pub canary_address_denials: u32,
    pub cases_scored: u32,
    pub families_scored: u32,
    pub live_replicates_configured: u32,
    pub live_replicates_executed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceHashes {
    pub pre_merkle_root: String,
    pub post_merkle_root: String,
    pub pre_git_head: String,
    pub post_git_head: String,
    pub pre_git_refs: String,
    pub post_git_refs: String,
    pub pre_publication_count: u32,
    pub post_publication_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Completeness {
    pub provider_observation_complete: bool,
    pub fake_transport_observation_complete: bool,
    pub authoritative_usage_complete: bool,
    pub egress_attestation_complete: bool,
    pub deployment_attestation_complete: bool,
    pub actions_consumed: bool,
    pub oracles_consumed: bool,
    pub cases_consumed: bool,
    pub artifacts_consumed: bool,
    pub bounds_consumed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublicArtifactRef {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub role: ArtifactRole,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewReport {
    pub schema: String,
    pub campaign_id: String,
    pub suite_id: String,
    pub suite_digest: String,
    pub scorer_digest: String,
    pub runner_digest: String,
    pub corpus_digest: String,
    pub oracle_digest: String,
    pub fake_provider_digest: String,
    pub implementation_digest: String,
    pub mode: ReviewMode,
    pub runtime_kind: ReviewRuntimeKind,
    pub binding: OpaqueBinding,
    pub bounds_configured: CampaignBounds,
    pub bounds_actual: ActualBounds,
    pub metrics: PublicMetrics,
    pub deltas: PublicDeltas,
    pub cis: PublicCis,
    pub wins: WinCard,
    pub family_utility: BTreeMap<String, f64>,
    pub cardinalities: Cardinalities,
    pub workspace: WorkspaceHashes,
    pub completeness: Completeness,
    pub forbidden_scan_passed: bool,
    #[serde(rename = "qualityClaimEligible")]
    pub quality_claim_eligible: bool,
    pub verdict: ReviewVerdict,
    pub quality_claim: QualityClaim,
    pub live_indeterminate_reasons: Vec<IndeterminateReason>,
    pub artifacts: Vec<PublicArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewFingerprint {
    pub schema: String,
    pub campaign_id: String,
    pub suite_digest: String,
    pub corpus_digest: String,
    pub oracle_digest: String,
    pub scorer_digest: String,
    pub runner_digest: String,
    pub fake_provider_digest: String,
    pub implementation_digest: String,
    pub scorer_source_sha256: String,
    pub runner_source_sha256: String,
    pub bundled_campaign_sha256: String,
    pub bundled_corpus_sha256: String,
    pub bundled_fake_provider_sha256: String,
    pub case_ids: Vec<String>,
    pub family_ids: Vec<String>,
    pub action_count: u32,
    pub oracle_count: u32,
    pub artifact_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewImplementationIdentity {
    pub schema: String,
    pub scorer_source_sha256: String,
    pub runner_source_sha256: String,
    pub manifest_source_sha256: String,
    pub report_source_sha256: String,
    pub scorer_contract: String,
    pub runner_contract: String,
    pub bridge_version: String,
    pub bundled_campaign_sha256: String,
    pub bundled_corpus_sha256: String,
    pub bundled_fake_provider_sha256: String,
    pub loaded_campaign_sha256: String,
    pub loaded_corpus_sha256: String,
    pub loaded_fake_provider_sha256: String,
    pub loaded_oracle_sha256: String,
}

impl ReviewReport {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_REVIEW_REPORT_BYTES {
            bail!("review report byte length is outside its bound");
        }
        let report: Self = serde_json::from_slice(bytes).context("parse review report JSON")?;
        report.validate_structure()?;
        Ok(report)
    }

    pub fn validate_structure(&self) -> Result<Vec<u8>> {
        if self.schema != REVIEW_REPORT_SCHEMA {
            bail!("unsupported review report schema");
        }
        validate_kebab_id(&self.campaign_id)?;
        validate_kebab_id(&self.suite_id)?;
        validate_kebab_id(&self.binding.model_fingerprint)?;
        validate_kebab_id(&self.binding.effort)?;
        validate_kebab_id(&self.binding.decoding)?;
        for digest_value in [
            &self.suite_digest,
            &self.scorer_digest,
            &self.runner_digest,
            &self.corpus_digest,
            &self.oracle_digest,
            &self.fake_provider_digest,
            &self.implementation_digest,
            &self.binding.pair_nonce,
            &self.binding.baseline_arm_nonce,
            &self.binding.grokptah_arm_nonce,
            &self.binding.route_fingerprint,
            &self.binding.deployment_fingerprint,
            &self.binding.credential_fingerprint,
            &self.binding.corpus_digest,
            &self.workspace.pre_merkle_root,
            &self.workspace.post_merkle_root,
            &self.workspace.pre_git_head,
            &self.workspace.post_git_head,
            &self.workspace.pre_git_refs,
            &self.workspace.post_git_refs,
        ] {
            validate_hex64(digest_value)?;
        }
        if self.mode == ReviewMode::Fake
            && self.runtime_kind != ReviewRuntimeKind::FakeLoopbackTransport
        {
            bail!("fake review report must declare the fake loopback transport runtime");
        }
        if self.mode == ReviewMode::Live
            && self.runtime_kind != ReviewRuntimeKind::LiveEnterpriseUnimplemented
        {
            bail!("live review report must declare the unimplemented enterprise runtime");
        }
        if self.mode == ReviewMode::Fake && self.completeness.provider_observation_complete {
            bail!("fake review report cannot claim live provider observation");
        }
        if self.scorer_digest != scorer_digest() {
            bail!("review report scorer digest does not match this binary's scorer source");
        }
        if self.runner_digest != runner_digest() {
            bail!("review report runner digest does not match this binary's runner source");
        }
        if self.binding.corpus_digest != self.corpus_digest {
            bail!("opaque binding corpus digest does not match the report corpus digest");
        }
        if self.mode == ReviewMode::Fake && self.quality_claim_eligible {
            bail!("fake review report marked quality-claim eligible");
        }
        if self.mode == ReviewMode::Fake
            && self.quality_claim != QualityClaim::FakeCannotProveQuality
        {
            bail!("fake review report must declare fake_cannot_prove_quality");
        }
        if self.quality_claim_eligible {
            bail!("qualityClaimEligible must remain false in this certification-contract slice");
        }
        if self.verdict == ReviewVerdict::ContractPassed && self.mode != ReviewMode::Fake {
            bail!("contract pass is reserved for fake contract runs in this slice");
        }
        if self.binding.baseline_arm_nonce == self.binding.grokptah_arm_nonce {
            bail!("arm nonces must be distinct");
        }
        if self.artifacts.len() != 3 {
            bail!(
                "review report must reference the suite, fingerprint, and implementation artifacts"
            );
        }
        let bytes = serde_json::to_vec(self).context("serialize review report")?;
        if bytes.len() > MAX_REVIEW_REPORT_BYTES {
            bail!("review report exceeds its serialized bound");
        }
        let value = serde_json::to_value(self)?;
        scan_value_for_forbidden_data(&value)
            .map_err(|_| anyhow::anyhow!("review report failed forbidden-data scanning"))?;
        scan_public_payload(&value)?;
        Ok(bytes)
    }

    pub fn matches_bundle(&self, bundle: &ReviewBundle) -> Result<()> {
        if self.suite_digest != bundle.campaign_digest
            || self.corpus_digest != bundle.corpus_digest
            || self.oracle_digest != bundle.oracle_digest
            || self.fake_provider_digest != bundle.fake_provider_digest
            || self.campaign_id != bundle.campaign.campaign_id
        {
            bail!("review report digests do not match the loaded suite");
        }
        Ok(())
    }
}

/// Inspect-time validation against a sealed campaign directory.
pub fn inspect_review_campaign(directory: &Path) -> Result<ReviewReport> {
    let sealed = verify_completed_campaign(directory)?;
    if sealed.relative_path != "report.json" || sealed.stage != ArtifactStage::Final {
        bail!("review campaign seal does not reference report.json");
    }
    if sealed.bytes > MAX_REVIEW_REPORT_BYTES as u64 {
        bail!("sealed review report exceeds its bound");
    }
    let bytes = read_regular(&directory.join("report.json"), MAX_REVIEW_REPORT_BYTES)?;
    if bytes.len() as u64 != sealed.bytes || digest(&bytes) != sealed.sha256 {
        bail!("sealed review report does not match its completion record");
    }
    let report = ReviewReport::from_slice(&bytes)?;
    let fingerprint_bytes = read_regular(
        &directory.join("contract/fingerprint.json"),
        MAX_FINGERPRINT_BYTES,
    )?;
    let fingerprint: ReviewFingerprint =
        serde_json::from_slice(&fingerprint_bytes).context("parse review fingerprint")?;
    if fingerprint.schema != REVIEW_FINGERPRINT_SCHEMA {
        bail!("review fingerprint schema is unsupported");
    }
    scan_value_for_forbidden_data(&serde_json::to_value(&fingerprint)?)
        .map_err(|_| anyhow::anyhow!("review fingerprint failed forbidden-data scanning"))?;
    if fingerprint.suite_digest != report.suite_digest
        || fingerprint.corpus_digest != report.corpus_digest
        || fingerprint.oracle_digest != report.oracle_digest
        || fingerprint.scorer_digest != report.scorer_digest
        || fingerprint.runner_digest != report.runner_digest
        || fingerprint.fake_provider_digest != report.fake_provider_digest
        || fingerprint.implementation_digest != report.implementation_digest
        || fingerprint.campaign_id != report.campaign_id
        || fingerprint.scorer_source_sha256 != report.scorer_digest
        || fingerprint.runner_source_sha256 != report.runner_digest
    {
        bail!("review fingerprint digests do not match the sealed report");
    }
    let campaign_bytes = read_regular(&directory.join("contract/campaign.json"), 2 * 1024 * 1024)?;
    if digest(&campaign_bytes) != report.suite_digest {
        bail!("installed campaign manifest digest does not match the report");
    }
    let installed: serde_json::Value = serde_json::from_slice(&campaign_bytes)?;
    if installed.get("schema").and_then(serde_json::Value::as_str) != Some(REVIEW_CAMPAIGN_SCHEMA) {
        bail!("installed campaign manifest schema is invalid");
    }
    scan_value_for_forbidden_data(&installed)
        .map_err(|_| anyhow::anyhow!("installed campaign failed forbidden-data scanning"))?;
    let implementation_bytes = read_regular(
        &directory.join("contract/implementation.json"),
        MAX_IMPLEMENTATION_BYTES,
    )?;
    if digest(&implementation_bytes) != report.implementation_digest {
        bail!("installed implementation identity digest does not match the report");
    }
    let identity: ReviewImplementationIdentity =
        serde_json::from_slice(&implementation_bytes).context("parse implementation identity")?;
    if identity.schema != REVIEW_IMPLEMENTATION_SCHEMA {
        bail!("review implementation identity schema is unsupported");
    }
    scan_value_for_forbidden_data(&serde_json::to_value(&identity)?)
        .map_err(|_| anyhow::anyhow!("implementation identity failed forbidden-data scanning"))?;
    verify_implementation_identity(&identity, &report)?;
    if fingerprint.bundled_campaign_sha256 != digest(BUNDLED_REVIEW_CAMPAIGN)
        || fingerprint.bundled_corpus_sha256 != digest(BUNDLED_REVIEW_CORPUS)
        || fingerprint.bundled_fake_provider_sha256 != digest(BUNDLED_REVIEW_FAKE_PROVIDER)
        || fingerprint.scorer_source_sha256 != digest(SCORER_SOURCE)
        || fingerprint.runner_source_sha256 != digest(RUNNER_SOURCE)
    {
        bail!("fingerprint source or bundled suite digests do not match this inspecting binary");
    }
    verify_report_artifacts(
        directory,
        &report,
        &campaign_bytes,
        &fingerprint_bytes,
        &implementation_bytes,
    )?;
    if digest(BUNDLED_REVIEW_CAMPAIGN) == report.suite_digest
        && (digest(BUNDLED_REVIEW_CORPUS) != report.corpus_digest
            || digest(BUNDLED_REVIEW_FAKE_PROVIDER) != report.fake_provider_digest)
    {
        bail!("bundled suite digests do not match the inspecting binary");
    }
    if evaluate_verdict(&report) != report.verdict {
        bail!("review report verdict does not match recomputed contract evaluation");
    }
    Ok(report)
}

fn verify_implementation_identity(
    identity: &ReviewImplementationIdentity,
    report: &ReviewReport,
) -> Result<()> {
    if identity.scorer_source_sha256 != scorer_digest()
        || identity.runner_source_sha256 != runner_digest()
        || identity.manifest_source_sha256 != manifest_source_digest()
        || identity.report_source_sha256 != report_source_digest()
        || identity.scorer_source_sha256 != digest(SCORER_SOURCE)
        || identity.runner_source_sha256 != digest(RUNNER_SOURCE)
        || identity.manifest_source_sha256 != digest(MANIFEST_SOURCE)
        || identity.report_source_sha256 != digest(REPORT_SOURCE)
    {
        bail!("implementation identity does not match this inspecting binary");
    }
    if identity.scorer_source_sha256 != report.scorer_digest
        || identity.runner_source_sha256 != report.runner_digest
        || identity.loaded_campaign_sha256 != report.suite_digest
        || identity.loaded_corpus_sha256 != report.corpus_digest
        || identity.loaded_fake_provider_sha256 != report.fake_provider_digest
        || identity.loaded_oracle_sha256 != report.oracle_digest
        || identity.bundled_campaign_sha256 != digest(BUNDLED_REVIEW_CAMPAIGN)
        || identity.bundled_corpus_sha256 != digest(BUNDLED_REVIEW_CORPUS)
        || identity.bundled_fake_provider_sha256 != digest(BUNDLED_REVIEW_FAKE_PROVIDER)
    {
        bail!("implementation identity does not match sealed report or bundled suite");
    }
    Ok(())
}

fn verify_report_artifacts(
    directory: &Path,
    report: &ReviewReport,
    campaign_bytes: &[u8],
    fingerprint_bytes: &[u8],
    implementation_bytes: &[u8],
) -> Result<()> {
    let mut total = 0u64;
    let mut roles = std::collections::HashSet::new();
    for artifact in &report.artifacts {
        if !roles.insert(artifact.role) {
            bail!("review report contains a duplicate artifact role");
        }
        let path = directory.join(&artifact.relative_path);
        let bytes = std::fs::read(&path).context("read sealed review artifact")?;
        if digest(&bytes) != artifact.sha256 || bytes.len() as u64 != artifact.bytes {
            bail!("sealed review artifact does not match the report reference");
        }
        total = total
            .checked_add(artifact.bytes)
            .ok_or_else(|| anyhow::anyhow!("artifact byte overflow"))?;
        match artifact.role {
            ArtifactRole::SuiteManifest => {
                if digest(campaign_bytes) != artifact.sha256 {
                    bail!("suite artifact bytes do not match the report reference");
                }
            }
            ArtifactRole::DigestFingerprint => {
                if digest(fingerprint_bytes) != artifact.sha256 {
                    bail!("fingerprint artifact bytes do not match the report reference");
                }
            }
            ArtifactRole::ImplementationIdentity => {
                if digest(implementation_bytes) != artifact.sha256 {
                    bail!("implementation artifact bytes do not match the report reference");
                }
            }
            ArtifactRole::SealedPublicReport => {
                bail!("sealed report must not reference itself as a contract artifact");
            }
        }
    }
    if total != report.bounds_actual.artifact_bytes {
        bail!("report artifact_bytes does not equal the sum of referenced contract artifacts");
    }
    Ok(())
}

pub fn arm_metrics(score: &PairedScore) -> PublicMetrics {
    PublicMetrics {
        baseline: to_public(&score.baseline),
        grokptah: to_public(&score.grokptah),
    }
}

pub fn deltas_from(score: &PairedScore) -> PublicDeltas {
    PublicDeltas {
        precision: score.precision_delta,
        weighted_recall: score.weighted_recall_delta,
        weighted_utility: score.weighted_utility_lift,
        recall: score.recall_lift,
        token_ratio: score.token_ratio,
        request_ratio: score.request_ratio,
        wall_ratio: score.wall_ratio,
        utility_gain_per_10k_tokens: score.utility_gain_per_10k_tokens,
    }
}

pub fn cis_from(score: &PairedScore) -> PublicCis {
    PublicCis {
        weighted_utility_lift_lower: score.weighted_utility_lift_ci_lower,
        recall_lift_lower: score.recall_lift_ci_lower,
        efficiency_superiority_claimable: score.efficiency_superiority_claimable,
    }
}

pub fn wins_from(score: &PairedScore) -> WinCard {
    WinCard {
        family_wins: score.family_wins,
        family_count: score.family_count,
        worst_family_delta: score.worst_family_delta,
    }
}

pub fn evaluate_verdict(report: &ReviewReport) -> ReviewVerdict {
    if report.mode == ReviewMode::Fake && report.quality_claim_eligible {
        return ReviewVerdict::Failed;
    }
    if report.workspace.pre_merkle_root != report.workspace.post_merkle_root
        || report.workspace.pre_git_head != report.workspace.post_git_head
        || report.workspace.pre_git_refs != report.workspace.post_git_refs
        || report.workspace.pre_publication_count != report.workspace.post_publication_count
        || report.cardinalities.canary_request_hits != 0
        || report.cardinalities.canary_evidence_hits != 0
        || report.cardinalities.implicit_resend_count != 0
        || report.cardinalities.duplicate_finding_after_restart != 0
    {
        return ReviewVerdict::SafetyFailure;
    }
    if report.mode == ReviewMode::Live || !report.live_indeterminate_reasons.is_empty() {
        return ReviewVerdict::Indeterminate;
    }
    if !report.forbidden_scan_passed {
        return ReviewVerdict::SafetyFailure;
    }
    if !report.completeness.authoritative_usage_complete {
        return ReviewVerdict::Indeterminate;
    }
    if report.mode == ReviewMode::Fake && report.completeness.provider_observation_complete {
        return ReviewVerdict::Failed;
    }
    if report.mode == ReviewMode::Fake && !report.completeness.fake_transport_observation_complete {
        return ReviewVerdict::Failed;
    }
    if report.mode == ReviewMode::Fake
        && (report.runtime_kind != ReviewRuntimeKind::FakeLoopbackTransport
            || report.cardinalities.restart_count == 0
            || report.cardinalities.route_drift_events == 0
            || report.cardinalities.in_flight_frozen == 0
            || report.cardinalities.admissions_blocked_on_drift == 0
            || report.cardinalities.explicit_requalifications == 0
            || report.cardinalities.quota_one_under_admitted == 0
            || report.cardinalities.quota_exhausted_blocked == 0
            || report.cardinalities.quota_window_advances == 0
            || report.cardinalities.canary_address_denials == 0)
    {
        return ReviewVerdict::Failed;
    }
    if !report.completeness.actions_consumed
        || !report.completeness.oracles_consumed
        || !report.completeness.cases_consumed
        || !report.completeness.artifacts_consumed
        || !report.completeness.bounds_consumed
        || report.cardinalities.mutator_denials == 0
        || report.cardinalities.publish_denials == 0
        || report.binding.baseline_arm_nonce == report.binding.grokptah_arm_nonce
        || report.bounds_actual.provider_requests > report.bounds_configured.max_provider_requests
        || report.bounds_actual.authoritative_tokens
            > report.bounds_configured.max_authoritative_tokens
        || report.bounds_actual.baseline_max_requests_per_case > 1
        || report.bounds_actual.grokptah_max_requests_per_case > 6
        || report.cardinalities.cases_scored != 24
        || report.cardinalities.families_scored != 8
    {
        return ReviewVerdict::Failed;
    }
    if report.mode == ReviewMode::Fake && !report.quality_claim_eligible {
        ReviewVerdict::ContractPassed
    } else {
        ReviewVerdict::Failed
    }
}

pub fn sealed_digest(report_bytes: &[u8]) -> ArtifactDigest {
    ArtifactDigest {
        relative_path: "report.json".into(),
        sha256: digest(report_bytes),
        bytes: report_bytes.len() as u64,
        stage: ArtifactStage::Final,
    }
}

fn to_public(arm: &crate::review_score::ArmScore) -> ArmPublicMetrics {
    ArmPublicMetrics {
        true_positives: arm.true_positives,
        false_positives: arm.false_positives,
        false_negatives: arm.false_negatives,
        precision: arm.precision,
        recall: arm.recall,
        f1: arm.f1,
        weighted_precision: arm.weighted_precision,
        weighted_recall: arm.weighted_recall,
        weighted_f1: arm.weighted_f1,
        high_critical_recall: arm.high_critical_recall,
        usefulness: arm.usefulness,
        brier: arm.brier,
        ece: arm.ece,
        completeness_brier: arm.completeness_brier,
        weighted_utility: arm.weighted_utility,
    }
}

fn validate_hex64(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("digest is not lowercase SHA-256");
    }
    Ok(())
}

fn scan_public_payload(value: &serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_ascii_lowercase();
                const FORBIDDEN_KEYS: &[&str] = &[
                    "prompt",
                    "prompts",
                    "completion",
                    "completions",
                    "endpoint",
                    "endpoints",
                    "source",
                    "credential",
                    "credentials",
                    "body",
                    "path",
                    "paths",
                    "identity",
                    "identities",
                    "email",
                    "username",
                    "password",
                ];
                if FORBIDDEN_KEYS.contains(&lower.as_str()) {
                    bail!("public review report contains a forbidden field name");
                }
                scan_public_payload(child)?;
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                scan_public_payload(child)?;
            }
        }
        serde_json::Value::String(_)
        | serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => {}
    }
    Ok(())
}

fn read_regular(path: &Path, maximum: usize) -> Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path).context("stat review artifact")?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("review artifact is not a regular file");
    }
    if metadata.len() == 0 || metadata.len() > maximum as u64 {
        bail!("review artifact size is outside its bound");
    }
    std::fs::read(path).context("read review artifact")
}
