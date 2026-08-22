//! Strict deny-unknown campaign, corpus, and fake-provider schemas for the
//! code-review benchmark. Ground-truth oracles stay in the corpus file and are
//! never part of the review-runtime view.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::scan_value_for_forbidden_data;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{REVIEW_CAMPAIGN_SCHEMA, REVIEW_CORPUS_SCHEMA, REVIEW_FAKE_PROVIDER_SCHEMA};

pub const BUNDLED_REVIEW_CAMPAIGN: &[u8] =
    include_bytes!("../../code-review-benchmark/campaign.v1.json");
pub const BUNDLED_REVIEW_CORPUS: &[u8] =
    include_bytes!("../../code-review-benchmark/corpus.v1.json");
pub const BUNDLED_REVIEW_FAKE_PROVIDER: &[u8] =
    include_bytes!("../../code-review-benchmark/fake-provider.v1.json");

pub const REVIEW_CAMPAIGN_RELATIVE_PATH: &str = "evals/code-review-benchmark/campaign.v1.json";
pub const EXPECTED_CASE_COUNT: usize = 24;
pub const EXPECTED_FAMILY_COUNT: usize = 8;
pub const EXPECTED_LIVE_REPLICATES: u32 = 2;
pub const EXPECTED_CATEGORIES_PER_LABEL: usize = 4;
pub const MAX_REVIEW_JSON_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_SYNTHETIC_BODY_BYTES: usize = 4 * 1024;
pub const SCORER_IDENTITY: &str = "grokptah.code-review-scorer.v1|one-to-one|tp=file+symbol+region+category+causal_atom|fp=duplicate+lure|severity-weighted|brier-ece|completeness";
pub const RUNNER_IDENTITY: &str =
    "grokptah.code-review-runner.v1|fake-contract|live-fail-closed-enterprise-lease";

const MAX_ID_BYTES: usize = 96;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReviewCampaign {
    pub schema: String,
    pub campaign_id: String,
    pub suite_id: String,
    pub held_out: bool,
    pub live_replicate_count: u32,
    pub corpus: CorpusReference,
    pub fake_provider: ProviderReference,
    pub pairing: PairingContract,
    pub prompt_cap_bytes: u64,
    pub response_cap_bytes: u64,
    pub arms: Vec<ArmContract>,
    pub denied_tools: Vec<ReviewTool>,
    pub bounds: CampaignBounds,
    pub thresholds: LiveThresholds,
    pub workspace_policy: WorkspacePolicy,
    pub actions: Vec<ReviewAction>,
    pub oracles: Vec<ReviewOracle>,
    pub artifacts: Vec<DeclaredArtifact>,
    pub families: Vec<FamilyRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusReference {
    pub relative_path: String,
    pub schema: String,
    pub corpus_id: String,
    pub corpus_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderReference {
    pub relative_path: String,
    pub schema: String,
    pub provider_id: String,
    pub provider_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PairingContract {
    pub same_deployment: bool,
    pub same_route: bool,
    pub same_credential: bool,
    pub same_model: bool,
    pub same_effort: bool,
    pub same_decoding: bool,
    pub same_corpus_digest: bool,
    pub same_prompt_cap: bool,
    pub same_response_cap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArmContract {
    pub id: ArmId,
    pub session_policy: SessionPolicy,
    pub max_requests_per_case: u32,
    pub max_tokens_per_case: u64,
    pub max_duration_seconds_per_case: u64,
    pub tools_allowed: Vec<ReviewTool>,
    pub memory_allowed: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ArmId {
    Baseline,
    Grokptah,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    FreshPerCase,
    PersistentPerFamily,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTool {
    ManifestRead,
    Search,
    ScopedMemory,
    Shell,
    Write,
    Mcp,
    Computer,
    Publish,
}

impl ReviewTool {
    pub fn forbidden_for_review(self) -> bool {
        matches!(
            self,
            Self::Shell | Self::Write | Self::Mcp | Self::Computer | Self::Publish
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CampaignBounds {
    pub max_provider_requests: u32,
    pub max_authoritative_tokens: u64,
    pub max_duration_seconds: u64,
    pub max_continuations: u32,
    pub max_artifact_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LiveThresholds {
    pub precision: f64,
    pub weighted_recall: f64,
    pub high_critical_recall: f64,
    pub usefulness: f64,
    pub brier: f64,
    pub ece: f64,
    pub paired_weighted_utility_lift: f64,
    pub paired_weighted_utility_lift_ci_lower: f64,
    pub recall_lift: f64,
    pub recall_lift_ci_lower: f64,
    pub family_wins: u32,
    pub family_count: u32,
    pub family_max_worse: f64,
    pub token_ratio: f64,
    pub request_ratio: f64,
    pub wall_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub kind: WorkspaceKind,
    pub writable: bool,
    pub disposable_required: bool,
    pub max_seed_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKind {
    SyntheticDisposable,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    MaterializeSyntheticWorkspace,
    SnapshotWorkspacePre,
    BindPairedArms,
    RunBaselineArm,
    RunGrokptahArm,
    RestartAfterDurableAdmission,
    ObserveRouteDrift,
    ObserveQuotaOneUnder,
    ObserveQuotaExhausted,
    ObserveQuotaWindowAdvance,
    DenyMutatorsAndPublish,
    ProveInaccessibleCanaryAbsent,
    SnapshotWorkspacePost,
    ScoreOneToOne,
    SealPublicReport,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ReviewOracle {
    WorkspaceMerkleUnchanged,
    GitRefsUnchanged,
    RemotePublicationUnchanged,
    BaselineExactlyOneRequest,
    GrokptahRequestBound,
    PairedBindingIdentical,
    NoForbiddenTools,
    RestartNoImplicitResend,
    RestartNoDuplicateFinding,
    RouteDriftFreezesInflight,
    RouteDriftBlocksUntilRequalified,
    QuotaOneUnderAdmits,
    QuotaExhaustedBlocks,
    QuotaWindowAdvanceResets,
    MutatorsDenied,
    PublishDenied,
    CanaryAbsentFromRequests,
    CanaryAbsentFromEvidence,
    FakeQualityIneligible,
    AuthoritativeUsagePresent,
    PublicRedactionPassed,
    ScorerOneToOne,
    ScriptedFindingsConsumed,
    DeclaredCasesConsumed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeclaredArtifact {
    pub id: String,
    pub relative_path: String,
    pub role: ArtifactRole,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactRole {
    SealedPublicReport,
    SuiteManifest,
    DigestFingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FamilyRef {
    pub id: String,
    pub case_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewCorpus {
    pub schema: String,
    pub corpus_id: String,
    pub corpus_version: u32,
    pub held_out: bool,
    pub inaccessible_canary: CanaryFile,
    pub families: Vec<CorpusFamily>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanaryFile {
    pub logical_name: String,
    pub body: String,
    pub readable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusFamily {
    pub id: String,
    pub evolution_order: Vec<String>,
    pub cases: Vec<CorpusCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorpusCase {
    pub id: String,
    pub held_out: bool,
    pub category: IssueCategory,
    pub severity: Severity,
    pub files: Vec<SyntheticFile>,
    pub hidden_oracle: HiddenOracle,
    pub lures: Vec<LureSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SyntheticFile {
    pub logical_name: String,
    pub language: String,
    pub body: String,
    pub readable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HiddenOracle {
    pub issue_id: String,
    pub logical_file: String,
    pub logical_symbol: String,
    pub accepted_region: LineRegion,
    pub causal_atom: String,
    pub usefulness_facts: UsefulnessFacts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LureSpec {
    pub logical_file: String,
    pub logical_symbol: String,
    pub start_line: u32,
    pub end_line: u32,
    pub category: IssueCategory,
    pub causal_atom: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct LineRegion {
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct UsefulnessFacts {
    pub cause: bool,
    pub impact: bool,
    pub remediation: bool,
    pub test: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IssueCategory {
    CorrectnessBounds,
    CrossFileDataflow,
    AuthInjectionSecrets,
    ConcurrencyRestartIdempotency,
    QuotaResourceBounds,
    ApiSchemaBackcompatDocsOps,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Low,
    Medium,
    High,
    Critical,
}

impl Severity {
    pub fn weight(self) -> u32 {
        match self {
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }

    pub fn high_or_critical(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FakeProviderManifest {
    pub schema: String,
    pub provider_id: String,
    pub provider_version: u32,
    pub binding: FakeBinding,
    pub quota: FakeQuota,
    pub adversarial_scenarios: Vec<AdversarialScenario>,
    pub denied_mutators: Vec<ReviewTool>,
    pub arm_scripts: Vec<ArmScript>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FakeBinding {
    pub route_class: RouteClass,
    pub tier: ModelTier,
    pub model_fingerprint: String,
    pub effort: String,
    pub decoding: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum RouteClass {
    CompatibleGateway,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    ApprovedModest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FakeQuota {
    pub window_requests: u32,
    pub one_under_remaining: u32,
    pub exhausted_remaining: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdversarialScenario {
    pub id: String,
    pub kind: AdversarialKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AdversarialKind {
    RestartAfterAdmission,
    RouteDrift,
    QuotaOneUnder,
    QuotaExhausted,
    QuotaWindowAdvance,
    MutatorPublishDenial,
    InaccessibleCanary,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArmScript {
    pub case_id: String,
    pub arm: ArmId,
    pub requests: Vec<ScriptedRequest>,
    pub findings: Vec<ScriptedFinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptedRequest {
    pub ordinal: u32,
    pub prompt_bytes: u64,
    pub completion_bytes: u64,
    pub authoritative_tokens: u64,
    pub tools: Vec<ReviewTool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScriptedFinding {
    pub logical_file: String,
    pub logical_symbol: String,
    pub start_line: u32,
    pub end_line: u32,
    pub category: IssueCategory,
    pub causal_atom: String,
    pub severity: Severity,
    pub confidence_millis: u32,
    pub usefulness: UsefulnessFacts,
}

#[derive(Debug, Clone)]
pub struct ReviewBundle {
    pub campaign: ReviewCampaign,
    pub corpus: ReviewCorpus,
    pub fake_provider: FakeProviderManifest,
    pub campaign_bytes: Vec<u8>,
    pub corpus_bytes: Vec<u8>,
    pub fake_provider_bytes: Vec<u8>,
    pub campaign_digest: String,
    pub corpus_digest: String,
    pub fake_provider_digest: String,
    pub oracle_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCorpus {
    pub families: Vec<RuntimeFamily>,
    pub canary_logical_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFamily {
    pub id: String,
    pub evolution_order: Vec<String>,
    pub cases: Vec<RuntimeCase>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCase {
    pub id: String,
    pub family_id: String,
    pub category: IssueCategory,
    pub severity: Severity,
    pub files: Vec<RuntimeFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFile {
    pub logical_name: String,
    pub opaque_file_id: String,
    pub content_sha256: String,
    pub bytes: u64,
    pub readable: bool,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenOracleSet {
    pub issues: Vec<ScoredOracle>,
    pub lures: Vec<ScoredLure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredOracle {
    pub case_id: String,
    pub family_id: String,
    pub issue_id: String,
    pub opaque_file_id: String,
    pub opaque_symbol_id: String,
    pub accepted_region: LineRegion,
    pub category: IssueCategory,
    pub causal_atom: String,
    pub severity: Severity,
    pub usefulness_facts: UsefulnessFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoredLure {
    pub case_id: String,
    pub opaque_file_id: String,
    pub opaque_symbol_id: String,
    pub region: LineRegion,
    pub category: IssueCategory,
    pub causal_atom: String,
}

impl ReviewCampaign {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        parse_strict::<Self>(bytes, REVIEW_CAMPAIGN_SCHEMA, "campaign")?.validated()
    }

    fn validated(self) -> Result<Self> {
        if self.schema != REVIEW_CAMPAIGN_SCHEMA {
            bail!("unsupported review campaign schema");
        }
        if !self.held_out {
            bail!("review campaign cases must be held-out");
        }
        validate_kebab_id(&self.campaign_id).context("campaign_id")?;
        validate_kebab_id(&self.suite_id).context("suite_id")?;
        if self.live_replicate_count != EXPECTED_LIVE_REPLICATES {
            bail!("review campaign must declare exactly two live replicates");
        }
        if self.corpus.relative_path != "corpus.v1.json"
            || self.corpus.schema != REVIEW_CORPUS_SCHEMA
            || self.corpus.corpus_id != "code-review-benchmark-corpus-v1"
            || self.corpus.corpus_version != 1
        {
            bail!("review campaign must bind the v1 synthetic corpus");
        }
        if self.fake_provider.relative_path != "fake-provider.v1.json"
            || self.fake_provider.schema != REVIEW_FAKE_PROVIDER_SCHEMA
            || self.fake_provider.provider_id != "code-review-benchmark-fake-provider-v1"
            || self.fake_provider.provider_version != 1
        {
            bail!("review campaign must bind the v1 fake provider");
        }
        validate_relative_file(&self.corpus.relative_path)?;
        validate_relative_file(&self.fake_provider.relative_path)?;
        if [
            self.pairing.same_deployment,
            self.pairing.same_route,
            self.pairing.same_credential,
            self.pairing.same_model,
            self.pairing.same_effort,
            self.pairing.same_decoding,
            self.pairing.same_corpus_digest,
            self.pairing.same_prompt_cap,
            self.pairing.same_response_cap,
        ]
        .iter()
        .any(|flag| !*flag)
        {
            bail!("paired arms must share the exact opaque binding");
        }
        if self.prompt_cap_bytes == 0 || self.prompt_cap_bytes > 64 * 1024 {
            bail!("prompt cap is outside its bound");
        }
        if self.response_cap_bytes == 0 || self.response_cap_bytes > 32 * 1024 {
            bail!("response cap is outside its bound");
        }
        if self.arms.len() != 2 {
            bail!("campaign must declare exactly the baseline and grokptah arms");
        }
        let baseline = self.arm(ArmId::Baseline)?;
        let grokptah = self.arm(ArmId::Grokptah)?;
        if baseline.session_policy != SessionPolicy::FreshPerCase
            || baseline.max_requests_per_case != 1
            || baseline.max_tokens_per_case != 8_000
            || baseline.max_duration_seconds_per_case != 120
            || !baseline.tools_allowed.is_empty()
            || baseline.memory_allowed
        {
            bail!("baseline arm contract is not the single-pass no-tool binding");
        }
        if grokptah.session_policy != SessionPolicy::PersistentPerFamily
            || grokptah.max_requests_per_case != 6
            || grokptah.max_tokens_per_case != 24_000
            || grokptah.max_duration_seconds_per_case != 600
            || grokptah.tools_allowed
                != [
                    ReviewTool::ManifestRead,
                    ReviewTool::Search,
                    ReviewTool::ScopedMemory,
                ]
            || !grokptah.memory_allowed
        {
            bail!("grokptah arm contract is not the persistent read/search/memory binding");
        }
        if self.denied_tools
            != [
                ReviewTool::Shell,
                ReviewTool::Write,
                ReviewTool::Mcp,
                ReviewTool::Computer,
                ReviewTool::Publish,
            ]
        {
            bail!("denied tool set must be shell/write/mcp/computer/publish");
        }
        if self.bounds.max_provider_requests != 400
            || self.bounds.max_authoritative_tokens != 1_250_000
            || self.bounds.max_duration_seconds != 28_800
            || self.bounds.max_continuations != 8
            || self.bounds.max_artifact_bytes != 128 * 1024 * 1024
        {
            bail!("campaign bounds must match the sealed review profile");
        }
        validate_thresholds(&self.thresholds)?;
        if self.workspace_policy.kind != WorkspaceKind::SyntheticDisposable
            || self.workspace_policy.writable
            || !self.workspace_policy.disposable_required
            || self.workspace_policy.max_seed_bytes == 0
            || self.workspace_policy.max_seed_bytes > 64 * 1024
        {
            bail!("workspace policy must be an immutable synthetic disposable fixture");
        }
        ensure_unique_copy(&self.actions, 32, "review action")?;
        ensure_unique_copy(&self.oracles, 64, "review oracle")?;
        if self.actions.len() != 15 || self.oracles.len() != 24 {
            bail!("review campaign must declare the complete action and oracle sets");
        }
        if self.artifacts.len() != 3 {
            bail!("review campaign must declare report, suite, and fingerprint artifacts");
        }
        let mut artifact_ids = HashSet::new();
        let mut artifact_paths = HashSet::new();
        let mut roles = HashSet::new();
        for artifact in &self.artifacts {
            validate_kebab_id(&artifact.id).context("artifact id")?;
            validate_relative_file(&artifact.relative_path)?;
            if !artifact_ids.insert(artifact.id.as_str())
                || !artifact_paths.insert(artifact.relative_path.as_str())
                || !roles.insert(artifact.role)
            {
                bail!("review campaign contains a duplicate artifact id, path, or role");
            }
        }
        if self.families.len() != EXPECTED_FAMILY_COUNT {
            bail!("review campaign must declare eight project families");
        }
        let mut family_ids = HashSet::new();
        let mut case_ids = HashSet::new();
        for family in &self.families {
            validate_kebab_id(&family.id).context("family id")?;
            if !family_ids.insert(family.id.as_str()) {
                bail!("review campaign contains a duplicate family id");
            }
            if family.case_ids.len() != 3 {
                bail!("each review family must declare exactly three cases");
            }
            for case_id in &family.case_ids {
                validate_kebab_id(case_id).context("case id")?;
                if !case_ids.insert(case_id.as_str()) {
                    bail!("review campaign contains a duplicate case id");
                }
            }
        }
        if case_ids.len() != EXPECTED_CASE_COUNT {
            bail!("review campaign must declare exactly 24 held-out cases");
        }
        Ok(self)
    }

    pub fn arm(&self, id: ArmId) -> Result<&ArmContract> {
        self.arms
            .iter()
            .find(|arm| arm.id == id)
            .ok_or_else(|| anyhow::anyhow!("review campaign is missing a required arm"))
    }

    pub fn artifact(&self, role: ArtifactRole) -> Result<&DeclaredArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.role == role)
            .ok_or_else(|| anyhow::anyhow!("review campaign is missing a declared artifact"))
    }

    pub fn case_ids(&self) -> Vec<String> {
        self.families
            .iter()
            .flat_map(|family| family.case_ids.clone())
            .collect()
    }
}

impl ReviewCorpus {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        parse_strict::<Self>(bytes, REVIEW_CORPUS_SCHEMA, "corpus")?.validated()
    }

    fn validated(self) -> Result<Self> {
        if self.schema != REVIEW_CORPUS_SCHEMA {
            bail!("unsupported review corpus schema");
        }
        if !self.held_out {
            bail!("review corpus must be held-out");
        }
        validate_kebab_id(&self.corpus_id)?;
        if self.corpus_id != "code-review-benchmark-corpus-v1" || self.corpus_version != 1 {
            bail!("review corpus identity is not the sealed v1 synthetic set");
        }
        validate_kebab_id(&self.inaccessible_canary.logical_name)?;
        if self.inaccessible_canary.readable || self.inaccessible_canary.body.is_empty() {
            bail!("inaccessible canary must be present and unreadable");
        }
        validate_body(&self.inaccessible_canary.body)?;
        if self.families.len() != EXPECTED_FAMILY_COUNT {
            bail!("review corpus must contain eight families");
        }
        let mut family_ids = HashSet::new();
        let mut case_ids = HashSet::new();
        let mut categories = BTreeMap::<IssueCategory, usize>::new();
        for family in &self.families {
            validate_kebab_id(&family.id)?;
            if !family_ids.insert(family.id.as_str()) {
                bail!("review corpus contains a duplicate family id");
            }
            if family.cases.len() != 3 || family.evolution_order.len() != 3 {
                bail!("each corpus family must have three ordered evolution cases");
            }
            let actual: Vec<_> = family.cases.iter().map(|case| case.id.as_str()).collect();
            let expected: Vec<_> = family.evolution_order.iter().map(String::as_str).collect();
            if actual != expected {
                bail!("corpus family evolution order must list each case once in order");
            }
            for case in &family.cases {
                validate_case(case)?;
                if !case_ids.insert(case.id.as_str()) {
                    bail!("review corpus contains a duplicate case id");
                }
                *categories.entry(case.category).or_insert(0) += 1;
            }
        }
        if case_ids.len() != EXPECTED_CASE_COUNT {
            bail!("review corpus must contain exactly 24 cases");
        }
        if categories.len() != 6
            || categories
                .values()
                .any(|count| *count != EXPECTED_CATEGORIES_PER_LABEL)
        {
            bail!("review corpus categories are not balanced across the six labels");
        }
        Ok(self)
    }

    pub fn split(&self) -> Result<(RuntimeCorpus, HiddenOracleSet)> {
        let mut families = Vec::with_capacity(self.families.len());
        let mut issues = Vec::new();
        let mut lures = Vec::new();
        for family in &self.families {
            let mut runtime_cases = Vec::new();
            for case in &family.cases {
                let mut files = Vec::new();
                for file in &case.files {
                    files.push(RuntimeFile {
                        logical_name: file.logical_name.clone(),
                        opaque_file_id: opaque_id(
                            "file",
                            &[&family.id, &case.id, &file.logical_name],
                        ),
                        content_sha256: digest(file.body.as_bytes()),
                        bytes: file.body.len() as u64,
                        readable: file.readable,
                        body: file.body.clone(),
                    });
                }
                let oracle_file = files
                    .iter()
                    .find(|file| file.logical_name == case.hidden_oracle.logical_file)
                    .ok_or_else(|| anyhow::anyhow!("hidden oracle references an unknown file"))?;
                issues.push(ScoredOracle {
                    case_id: case.id.clone(),
                    family_id: family.id.clone(),
                    issue_id: case.hidden_oracle.issue_id.clone(),
                    opaque_file_id: oracle_file.opaque_file_id.clone(),
                    opaque_symbol_id: opaque_id(
                        "sym",
                        &[&family.id, &case.id, &case.hidden_oracle.logical_symbol],
                    ),
                    accepted_region: case.hidden_oracle.accepted_region,
                    category: case.category,
                    causal_atom: case.hidden_oracle.causal_atom.clone(),
                    severity: case.severity,
                    usefulness_facts: case.hidden_oracle.usefulness_facts,
                });
                for lure in &case.lures {
                    let lure_file = files
                        .iter()
                        .find(|file| file.logical_name == lure.logical_file)
                        .ok_or_else(|| anyhow::anyhow!("lure references an unknown file"))?;
                    lures.push(ScoredLure {
                        case_id: case.id.clone(),
                        opaque_file_id: lure_file.opaque_file_id.clone(),
                        opaque_symbol_id: opaque_id(
                            "sym",
                            &[&family.id, &case.id, &lure.logical_symbol],
                        ),
                        region: LineRegion {
                            start_line: lure.start_line,
                            end_line: lure.end_line,
                        },
                        category: lure.category,
                        causal_atom: lure.causal_atom.clone(),
                    });
                }
                runtime_cases.push(RuntimeCase {
                    id: case.id.clone(),
                    family_id: family.id.clone(),
                    category: case.category,
                    severity: case.severity,
                    files,
                });
            }
            families.push(RuntimeFamily {
                id: family.id.clone(),
                evolution_order: family.evolution_order.clone(),
                cases: runtime_cases,
            });
        }
        Ok((
            RuntimeCorpus {
                families,
                canary_logical_name: self.inaccessible_canary.logical_name.clone(),
            },
            HiddenOracleSet { issues, lures },
        ))
    }
}

impl FakeProviderManifest {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        parse_strict::<Self>(bytes, REVIEW_FAKE_PROVIDER_SCHEMA, "fake provider")?.validated()
    }

    fn validated(self) -> Result<Self> {
        if self.schema != REVIEW_FAKE_PROVIDER_SCHEMA {
            bail!("unsupported fake provider schema");
        }
        validate_kebab_id(&self.provider_id)?;
        if self.provider_id != "code-review-benchmark-fake-provider-v1"
            || self.provider_version != 1
        {
            bail!("fake provider identity is not the sealed v1 fixture");
        }
        validate_kebab_id(&self.binding.model_fingerprint)?;
        validate_kebab_id(&self.binding.effort)?;
        validate_kebab_id(&self.binding.decoding)?;
        if self.quota.window_requests != 400
            || self.quota.one_under_remaining != 1
            || self.quota.exhausted_remaining != 0
        {
            bail!("fake quota fixture is not the sealed one-under/exhausted/window profile");
        }
        if self.adversarial_scenarios.len() != 7 {
            bail!("fake provider must declare the complete adversarial scenario set");
        }
        let mut scenario_ids = HashSet::new();
        let mut kinds = HashSet::new();
        for scenario in &self.adversarial_scenarios {
            validate_kebab_id(&scenario.id)?;
            if !scenario_ids.insert(scenario.id.as_str()) || !kinds.insert(scenario.kind) {
                bail!("fake provider contains a duplicate adversarial scenario");
            }
        }
        if self.denied_mutators
            != [
                ReviewTool::Shell,
                ReviewTool::Write,
                ReviewTool::Mcp,
                ReviewTool::Computer,
                ReviewTool::Publish,
            ]
        {
            bail!("fake provider must deny every mutator and publish tool");
        }
        if self.arm_scripts.len() != EXPECTED_CASE_COUNT * 2 {
            bail!("fake provider must script both arms for every case");
        }
        let mut seen = HashSet::new();
        for script in &self.arm_scripts {
            validate_kebab_id(&script.case_id)?;
            if !seen.insert((script.case_id.as_str(), script.arm)) {
                bail!("fake provider contains a duplicate arm script");
            }
            if script.requests.is_empty() {
                bail!("fake provider arm script must declare at least one request");
            }
            match script.arm {
                ArmId::Baseline => {
                    if script.requests.len() != 1 {
                        bail!("baseline script must emit exactly one request");
                    }
                    if script
                        .requests
                        .iter()
                        .any(|request| !request.tools.is_empty())
                    {
                        bail!("baseline script must not use tools");
                    }
                }
                ArmId::Grokptah => {
                    if script.requests.len() > 6 {
                        bail!("grokptah script exceeds the per-case request bound");
                    }
                    if script
                        .requests
                        .iter()
                        .any(|request| request.tools.iter().any(|tool| tool.forbidden_for_review()))
                    {
                        bail!("grokptah script uses a forbidden tool");
                    }
                }
            }
            let mut ordinals = HashSet::new();
            for request in &script.requests {
                if request.ordinal == 0 || !ordinals.insert(request.ordinal) {
                    bail!("fake provider request ordinals must be unique and 1-based");
                }
                if request.prompt_bytes == 0 || request.completion_bytes == 0 {
                    bail!("scripted request must include prompt and completion sizes");
                }
                if request.authoritative_tokens == 0 {
                    bail!("scripted request is missing authoritative usage");
                }
            }
            for finding in &script.findings {
                validate_kebab_id(&finding.logical_file)?;
                validate_kebab_id(&finding.logical_symbol)?;
                validate_kebab_id(&finding.causal_atom)?;
                if finding.start_line == 0
                    || finding.end_line == 0
                    || finding.start_line > finding.end_line
                {
                    bail!("scripted finding region is invalid");
                }
                if finding.confidence_millis > 1_000 {
                    bail!("scripted confidence is outside 0..=1000 millis");
                }
            }
        }
        Ok(self)
    }

    pub fn script(&self, case_id: &str, arm: ArmId) -> Result<&ArmScript> {
        self.arm_scripts
            .iter()
            .find(|script| script.case_id == case_id && script.arm == arm)
            .ok_or_else(|| anyhow::anyhow!("fake provider is missing a required arm script"))
    }
}

impl ReviewBundle {
    pub fn bundled() -> Result<Self> {
        Self::from_parts(
            BUNDLED_REVIEW_CAMPAIGN,
            BUNDLED_REVIEW_CORPUS,
            BUNDLED_REVIEW_FAKE_PROVIDER,
        )
    }

    pub fn load_checked(campaign_path: &Path, repository_root: &Path) -> Result<Self> {
        reject_symlink_components(repository_root, campaign_path)?;
        let root = dunce::canonicalize(repository_root).context("canonicalize repository root")?;
        let campaign_path =
            dunce::canonicalize(campaign_path).context("canonicalize review campaign path")?;
        if !campaign_path.starts_with(&root) {
            bail!("review campaign escapes repository root");
        }
        let campaign_bytes = read_bounded(&campaign_path, MAX_REVIEW_JSON_BYTES)?;
        let campaign = ReviewCampaign::from_slice(&campaign_bytes)?;
        let parent = campaign_path
            .parent()
            .context("review campaign has no parent")?;
        let corpus_path = parent.join(&campaign.corpus.relative_path);
        let provider_path = parent.join(&campaign.fake_provider.relative_path);
        reject_symlink(&corpus_path)?;
        reject_symlink(&provider_path)?;
        let corpus_path = dunce::canonicalize(&corpus_path).context("canonicalize corpus")?;
        let provider_path =
            dunce::canonicalize(&provider_path).context("canonicalize fake provider")?;
        if !corpus_path.starts_with(&root) || !provider_path.starts_with(&root) {
            bail!("review corpus or fake provider escapes repository root");
        }
        let corpus_bytes = read_bounded(&corpus_path, MAX_REVIEW_JSON_BYTES)?;
        let provider_bytes = read_bounded(&provider_path, MAX_REVIEW_JSON_BYTES)?;
        Self::from_parts(&campaign_bytes, &corpus_bytes, &provider_bytes)
    }

    fn from_parts(campaign: &[u8], corpus: &[u8], provider: &[u8]) -> Result<Self> {
        let parsed_campaign = ReviewCampaign::from_slice(campaign)?;
        let parsed_corpus = ReviewCorpus::from_slice(corpus)?;
        let parsed_provider = FakeProviderManifest::from_slice(provider)?;
        bind_bundle(&parsed_campaign, &parsed_corpus, &parsed_provider)?;
        let oracle_digest = oracle_digest(&parsed_corpus)?;
        Ok(Self {
            campaign: parsed_campaign,
            corpus: parsed_corpus,
            fake_provider: parsed_provider,
            campaign_bytes: campaign.to_vec(),
            corpus_bytes: corpus.to_vec(),
            fake_provider_bytes: provider.to_vec(),
            campaign_digest: digest(campaign),
            corpus_digest: digest(corpus),
            fake_provider_digest: digest(provider),
            oracle_digest,
        })
    }
}

pub fn scorer_digest() -> String {
    digest(SCORER_IDENTITY.as_bytes())
}

pub fn runner_digest() -> String {
    digest(RUNNER_IDENTITY.as_bytes())
}

pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub fn opaque_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(prefix.as_bytes());
    for part in parts {
        hasher.update([0]);
        hasher.update(part.as_bytes());
    }
    format!("{prefix}-{:x}", hasher.finalize())
        .chars()
        .take(21)
        .collect()
}

pub fn validate_kebab_id(value: &str) -> Result<()> {
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

fn bind_bundle(
    campaign: &ReviewCampaign,
    corpus: &ReviewCorpus,
    provider: &FakeProviderManifest,
) -> Result<()> {
    if campaign.corpus.corpus_id != corpus.corpus_id
        || campaign.fake_provider.provider_id != provider.provider_id
    {
        bail!("campaign references do not match loaded corpus or fake provider identities");
    }
    if campaign.families.len() != corpus.families.len() {
        bail!("campaign and corpus family counts differ");
    }
    for (declared, actual) in campaign.families.iter().zip(&corpus.families) {
        if declared.id != actual.id || declared.case_ids != actual.evolution_order {
            bail!("campaign family case set does not match the corpus evolution order");
        }
        let corpus_ids: Vec<_> = actual.cases.iter().map(|case| case.id.clone()).collect();
        if declared.case_ids != corpus_ids {
            bail!("campaign case order does not match corpus case order");
        }
    }
    let mut unused_scripts: BTreeSet<(String, ArmId)> = provider
        .arm_scripts
        .iter()
        .map(|script| (script.case_id.clone(), script.arm))
        .collect();
    for family in &campaign.families {
        for case_id in &family.case_ids {
            for arm in [ArmId::Baseline, ArmId::Grokptah] {
                if !unused_scripts.remove(&(case_id.clone(), arm)) {
                    bail!("fake provider is missing a script for a declared case");
                }
                let script = provider.script(case_id, arm)?;
                let case = corpus
                    .families
                    .iter()
                    .flat_map(|item| item.cases.iter())
                    .find(|item| item.id == *case_id)
                    .context("declared case missing from corpus")?;
                for finding in &script.findings {
                    if !case
                        .files
                        .iter()
                        .any(|file| file.logical_name == finding.logical_file)
                    {
                        bail!("scripted finding references a file absent from its case");
                    }
                }
            }
        }
    }
    if !unused_scripts.is_empty() {
        bail!("fake provider declares a script that no campaign case consumes");
    }
    Ok(())
}

fn validate_case(case: &CorpusCase) -> Result<()> {
    validate_kebab_id(&case.id)?;
    if !case.held_out {
        bail!("corpus case must be held-out");
    }
    if case.files.len() != 1 {
        bail!("each corpus case must declare exactly one readable synthetic file");
    }
    let file = &case.files[0];
    validate_kebab_id(&file.logical_name)?;
    if file.language != "rust" || !file.readable {
        bail!("synthetic review files must be readable rust snippets");
    }
    validate_body(&file.body)?;
    validate_kebab_id(&case.hidden_oracle.issue_id)?;
    validate_kebab_id(&case.hidden_oracle.logical_file)?;
    validate_kebab_id(&case.hidden_oracle.logical_symbol)?;
    validate_kebab_id(&case.hidden_oracle.causal_atom)?;
    if case.hidden_oracle.logical_file != file.logical_name {
        bail!("hidden oracle file does not match the case file");
    }
    validate_region(case.hidden_oracle.accepted_region)?;
    if case.lures.len() != 1 {
        bail!("each case must declare exactly one lure");
    }
    let lure = &case.lures[0];
    validate_kebab_id(&lure.logical_file)?;
    validate_kebab_id(&lure.logical_symbol)?;
    validate_kebab_id(&lure.causal_atom)?;
    if lure.causal_atom == case.hidden_oracle.causal_atom {
        bail!("lure causal atom must not equal the true issue atom");
    }
    validate_region(LineRegion {
        start_line: lure.start_line,
        end_line: lure.end_line,
    })?;
    if !case.hidden_oracle.usefulness_facts.cause
        || !case.hidden_oracle.usefulness_facts.impact
        || !case.hidden_oracle.usefulness_facts.remediation
        || !case.hidden_oracle.usefulness_facts.test
    {
        bail!("hidden oracle must require all four usefulness facts");
    }
    Ok(())
}

fn validate_region(region: LineRegion) -> Result<()> {
    if region.start_line == 0 || region.end_line == 0 || region.start_line > region.end_line {
        bail!("line region is invalid");
    }
    Ok(())
}

fn validate_body(body: &str) -> Result<()> {
    if body.is_empty()
        || body.len() > MAX_SYNTHETIC_BODY_BYTES
        || body.chars().any(char::is_control)
    {
        bail!("synthetic body is empty, oversized, or contains a control character");
    }
    Ok(())
}

fn validate_thresholds(thresholds: &LiveThresholds) -> Result<()> {
    let rates = [
        thresholds.precision,
        thresholds.weighted_recall,
        thresholds.high_critical_recall,
        thresholds.usefulness,
        thresholds.brier,
        thresholds.ece,
        thresholds.paired_weighted_utility_lift,
        thresholds.paired_weighted_utility_lift_ci_lower,
        thresholds.recall_lift,
        thresholds.recall_lift_ci_lower,
        thresholds.family_max_worse,
    ];
    if rates
        .iter()
        .any(|value| !value.is_finite() || *value < 0.0 || *value > 1.0)
    {
        bail!("live quality threshold is outside 0..=1");
    }
    if (thresholds.precision - 0.75).abs() > f64::EPSILON
        || (thresholds.weighted_recall - 0.75).abs() > f64::EPSILON
        || (thresholds.high_critical_recall - 0.85).abs() > f64::EPSILON
        || (thresholds.usefulness - 0.70).abs() > f64::EPSILON
        || (thresholds.brier - 0.20).abs() > f64::EPSILON
        || (thresholds.ece - 0.15).abs() > f64::EPSILON
        || (thresholds.paired_weighted_utility_lift - 0.15).abs() > f64::EPSILON
        || (thresholds.paired_weighted_utility_lift_ci_lower - 0.08).abs() > f64::EPSILON
        || (thresholds.recall_lift - 0.15).abs() > f64::EPSILON
        || (thresholds.recall_lift_ci_lower - 0.05).abs() > f64::EPSILON
        || (thresholds.family_max_worse - 0.10).abs() > f64::EPSILON
        || thresholds.family_wins != 6
        || thresholds.family_count != 8
        || (thresholds.token_ratio - 6.0).abs() > f64::EPSILON
        || (thresholds.request_ratio - 6.0).abs() > f64::EPSILON
        || (thresholds.wall_ratio - 5.0).abs() > f64::EPSILON
    {
        bail!("live quality thresholds must match the sealed review contract");
    }
    Ok(())
}

fn oracle_digest(corpus: &ReviewCorpus) -> Result<String> {
    let mut hasher = Sha256::new();
    for family in &corpus.families {
        for case in &family.cases {
            hasher.update(case.hidden_oracle.issue_id.as_bytes());
            hasher.update([0]);
            hasher.update(case.hidden_oracle.causal_atom.as_bytes());
            hasher.update([0]);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn parse_strict<T>(bytes: &[u8], schema: &str, label: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.is_empty() || bytes.len() > MAX_REVIEW_JSON_BYTES {
        bail!("{label} byte length is outside its bound");
    }
    let value: T = serde_json::from_slice(bytes).with_context(|| format!("parse {label} JSON"))?;
    let json = serde_json::to_value(&value)?;
    if json.get("schema").and_then(serde_json::Value::as_str) != Some(schema) {
        bail!("{label} schema mismatch");
    }
    scan_value_for_forbidden_data(&json)
        .map_err(|_| anyhow::anyhow!("{label} failed forbidden-data scanning"))?;
    Ok(value)
}

fn ensure_unique_copy<T>(values: &[T], max: usize, name: &str) -> Result<()>
where
    T: Copy + Eq + std::hash::Hash,
{
    if values.is_empty() || values.len() > max {
        bail!("{name} count is outside its bound");
    }
    let mut unique = HashSet::new();
    for value in values {
        if !unique.insert(*value) {
            bail!("review campaign contains a duplicate {name}");
        }
    }
    Ok(())
}

fn validate_relative_file(value: &str) -> Result<()> {
    let path = Path::new(value);
    if path.is_absolute() || value.contains('\\') || value.is_empty() {
        bail!("relative file path is invalid");
    }
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                if part.is_empty() || part == ".git" {
                    bail!("relative file path contains a reserved component");
                }
            }
            _ => bail!("relative file path contains a traversal component"),
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
    if !opened.is_file() || opened.len() != metadata.len() {
        bail!("bounded JSON file changed while opening");
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .context("read bounded JSON file")?;
    if bytes.len() != opened.len() as usize {
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

pub fn default_campaign_path(repository: &Path) -> PathBuf {
    repository.join(REVIEW_CAMPAIGN_RELATIVE_PATH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn campaign_value() -> Value {
        serde_json::from_slice(BUNDLED_REVIEW_CAMPAIGN).unwrap()
    }

    #[test]
    fn bundled_review_bundle_validates() {
        let bundle = ReviewBundle::bundled().unwrap();
        assert_eq!(bundle.campaign.case_ids().len(), 24);
        assert_eq!(bundle.corpus.families.len(), 8);
        let (runtime, oracles) = bundle.corpus.split().unwrap();
        assert_eq!(oracles.issues.len(), 24);
        assert_eq!(oracles.lures.len(), 24);
        assert!(runtime
            .families
            .iter()
            .flat_map(|family| &family.cases)
            .all(|case| case
                .files
                .iter()
                .all(|file| file.opaque_file_id.starts_with("file-"))));
        assert!(!format!("{runtime:?}").contains("hidden_oracle"));
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let mut value = campaign_value();
        value["unexpected"] = json!(true);
        assert!(ReviewCampaign::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());
    }

    #[test]
    fn duplicate_case_fails_closed() {
        let mut value = campaign_value();
        value["families"][0]["case_ids"][1] = value["families"][0]["case_ids"][0].clone();
        assert!(ReviewCampaign::from_slice(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
