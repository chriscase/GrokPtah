use std::collections::{BTreeSet, HashMap};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::certification::PERSISTENT_AGENT_SCENARIO_IDS;
use grokptah_agent_bridge::{
    public_xai_endpoint_fingerprint, scan_value_for_forbidden_data,
    AttemptDisposition as CaptureAttemptDisposition, PersistentAgentCapture,
    StreamFraming as CaptureFraming, PERSISTENT_AGENT_CAPTURE_SCHEMA,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::report::validate_safe_id;
use crate::{REPLAY_FIXTURE_SCHEMA, REVIEW_CANDIDATE_SCHEMA};

pub const MAX_FIXTURE_BYTES: usize = 512 * 1024;
pub const MAX_FIXTURE_CASES: usize = 128;
pub const MAX_RECORDS_PER_CASE: usize = 2_048;
pub const NORMALIZER_VERSION: &str = "grokptah-cert-normalizer-v1";
const MAX_REPLAY_TOKENS: u64 = 2_000_000;
const RETAINED_FIELDS: &[&str] = &[
    "attempt.ordinal",
    "attempt.disposition",
    "attempt.framing",
    "attempt.usage_shape",
    "durable_state.terminal_category",
];
const TRANSFORMATIONS: &[&str] = &[
    "dynamic_identifiers_omitted",
    "timestamps_omitted",
    "status_values_coarsened",
    "canonical_record_order",
];
const OMISSIONS: &[&str] = &[
    "credentials",
    "headers",
    "urls",
    "paths",
    "prompts",
    "completions",
    "reasoning",
    "artifact_bodies",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayFixtureKind {
    SyntheticBehavior,
    LiveCaptureStructural,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayAttemptDisposition {
    Completed,
    Retried,
    Downgraded,
    RateLimited,
    TimedOut,
    TransportFailed,
    ProviderRejected,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayFraming {
    Sse,
    Json,
    NoBody,
    TransportFailure,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SyntheticTool {
    ReadFixture,
    WriteMarker,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ArgumentShape {
    EmptyObject,
    BoundedObject,
    MalformedJson,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allowed,
    Denied,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCategory {
    Completed,
    Failed,
    ProviderError,
    Timeout,
    Interrupted,
    LimitReached,
    TokenCeiling,
    RateLimitExhausted,
    PermissionDenied,
    MalformedToolArguments,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UsageShape {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReplayRecord {
    ProviderAttempt {
        ordinal: u32,
        disposition: ReplayAttemptDisposition,
        framing: ReplayFraming,
        #[serde(skip_serializing_if = "Option::is_none")]
        usage: Option<UsageShape>,
    },
    Backoff {
        ordinal: u32,
        attempt: u32,
    },
    ToolCall {
        ordinal: u32,
        call_id: u32,
        tool: SyntheticTool,
        argument_shape: ArgumentShape,
    },
    PermissionRequired {
        ordinal: u32,
        call_id: u32,
        decision: PermissionDecision,
    },
    ToolResult {
        ordinal: u32,
        call_id: u32,
        outcome: ToolOutcome,
    },
    ResponseDelta {
        ordinal: u32,
        response_id: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        duplicate_of: Option<u32>,
    },
    DurableTerminal {
        ordinal: u32,
        category: TerminalCategory,
    },
    Finish {
        ordinal: u32,
        category: TerminalCategory,
    },
}

impl ReplayRecord {
    fn ordinal(&self) -> u32 {
        match self {
            Self::ProviderAttempt { ordinal, .. }
            | Self::Backoff { ordinal, .. }
            | Self::ToolCall { ordinal, .. }
            | Self::PermissionRequired { ordinal, .. }
            | Self::ToolResult { ordinal, .. }
            | Self::ResponseDelta { ordinal, .. }
            | Self::DurableTerminal { ordinal, .. }
            | Self::Finish { ordinal, .. } => *ordinal,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayCounters {
    pub provider_attempts: u32,
    pub tool_calls: u32,
    pub tool_executions: u32,
    pub permission_requests: u32,
    pub response_deltas_applied: u32,
    pub duplicates_ignored: u32,
    pub backoffs: u32,
    pub errors: u32,
    pub total_tokens: u64,
    pub usage_complete: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayExpected {
    pub terminal: TerminalCategory,
    pub counters: ReplayCounters,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayCase {
    pub id: String,
    pub catalog_scenario_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_run_id: Option<String>,
    pub records: Vec<ReplayRecord>,
    pub expected: ReplayExpected,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayFixture {
    pub schema: String,
    pub fixture_id: String,
    pub kind: ReplayFixtureKind,
    pub cases: Vec<ReplayCase>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDiagnosticCode {
    OracleMatch,
    OracleMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayCaseResult {
    pub id: String,
    pub terminal: TerminalCategory,
    pub counters: ReplayCounters,
    pub passed: bool,
    pub diagnostic_code: ReplayDiagnosticCode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReplayResult {
    pub fixture_sha256: String,
    pub cases: Vec<ReplayCaseResult>,
    pub passed: bool,
    pub oracle_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PromotionManifest {
    pub source_capture_sha256: String,
    pub source_campaign_id: String,
    pub source_run_ids: Vec<String>,
    pub catalog_scenario_id: String,
    pub normalizer_version: String,
    pub retained_fields: Vec<String>,
    pub transformations: Vec<String>,
    pub omissions: Vec<String>,
    pub fixture_sha256: String,
    pub oracle_sha256: String,
    pub automatic_promotion: bool,
    pub human_review_required: bool,
    pub structural_only: bool,
    pub fixture_promotion_eligible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewCandidate {
    pub schema: String,
    pub candidate_id: String,
    pub source_capture_schema: String,
    pub fixture: ReplayFixture,
    pub promotion_manifest: PromotionManifest,
}

impl ReplayFixture {
    pub fn validate(&self) -> Result<Vec<u8>> {
        if self.schema != REPLAY_FIXTURE_SCHEMA {
            bail!("unsupported replay fixture schema");
        }
        validate_safe_id(&self.fixture_id).context("fixture_id")?;
        if self.cases.is_empty() || self.cases.len() > MAX_FIXTURE_CASES {
            bail!("replay fixture case count is outside its bound");
        }
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            validate_safe_id(&case.id).context("case id")?;
            validate_safe_id(&case.catalog_scenario_id).context("catalog scenario id")?;
            if !PERSISTENT_AGENT_SCENARIO_IDS.contains(&case.catalog_scenario_id.as_str()) {
                bail!("replay case references an unknown authoritative catalog scenario");
            }
            match (self.kind, &case.source_run_id) {
                (ReplayFixtureKind::SyntheticBehavior, None) => {}
                (ReplayFixtureKind::LiveCaptureStructural, Some(value)) => {
                    validate_opaque_label(value)?;
                }
                _ => bail!("replay case Run partition does not match its fixture kind"),
            }
            if !ids.insert(case.id.as_str()) {
                bail!("duplicate replay case id");
            }
            if case.records.is_empty() || case.records.len() > MAX_RECORDS_PER_CASE {
                bail!("replay record count is outside its bound");
            }
            for (index, record) in case.records.iter().enumerate() {
                if record.ordinal() as usize != index + 1 {
                    bail!("replay record ordinals are not contiguous");
                }
                if self.kind == ReplayFixtureKind::LiveCaptureStructural
                    && !matches!(
                        record,
                        ReplayRecord::ProviderAttempt { .. }
                            | ReplayRecord::DurableTerminal { .. }
                            | ReplayRecord::Finish { .. }
                    )
                {
                    bail!("live-capture normalization contains synthetic payload behavior");
                }
            }
            validate_replay_counters(&case.expected.counters)?;
        }
        let value = serde_json::to_value(self)?;
        scan_value_for_forbidden_data(&value)
            .map_err(|_| anyhow::anyhow!("fixture failed forbidden-data scanning"))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        if bytes.len() > MAX_FIXTURE_BYTES {
            bail!("replay fixture exceeds the serialized byte bound");
        }
        Ok(bytes)
    }
}

pub fn replay_fixture(fixture: &ReplayFixture) -> Result<ReplayResult> {
    let fixture_bytes = fixture.validate()?;
    let fixture_sha256 = digest(&fixture_bytes);
    let mut results = Vec::with_capacity(fixture.cases.len());
    for case in &fixture.cases {
        results.push(replay_case(fixture.kind, case)?);
    }
    let passed = results.iter().all(|result| result.passed);
    let canonical = serde_json::to_vec(&results)?;
    let oracle_sha256 = digest(&canonical);
    Ok(ReplayResult {
        fixture_sha256,
        cases: results,
        passed,
        oracle_sha256,
    })
}

fn replay_case(kind: ReplayFixtureKind, case: &ReplayCase) -> Result<ReplayCaseResult> {
    let mut counters = ReplayCounters {
        usage_complete: true,
        ..ReplayCounters::default()
    };
    let mut pending_tools = HashMap::<u32, (ArgumentShape, Option<PermissionDecision>)>::new();
    let mut executed_tools = BTreeSet::new();
    let mut deltas = BTreeSet::new();
    let mut terminal = None;
    let mut rate_limit_needs_backoff = None;
    let mut durable_terminal = None;
    let mut successful_attempt = false;
    let mut provider_error_attempt = false;
    let mut timeout_attempt = false;
    let mut interrupted_attempt = false;
    let mut last_disposition = None;

    for record in &case.records {
        if terminal.is_some() {
            bail!("replay case contains records after its terminal record");
        }
        match record {
            ReplayRecord::ProviderAttempt {
                disposition,
                framing,
                usage,
                ..
            } => {
                // A later attempt proves retry succession, but not that a
                // backoff occurred. Only an explicit Backoff record carries
                // that stronger synthetic-fixture claim.
                if rate_limit_needs_backoff.is_some()
                    && kind == ReplayFixtureKind::SyntheticBehavior
                {
                    bail!("synthetic rate-limit retry has no explicit backoff evidence");
                }
                rate_limit_needs_backoff = None;
                validate_attempt_framing(*disposition, *framing)?;
                increment(&mut counters.provider_attempts)?;
                if let Some(usage) = usage {
                    let sum = usage
                        .input_tokens
                        .checked_add(usage.output_tokens)
                        .context("fixture usage overflow")?;
                    if usage.complete && sum != usage.total_tokens {
                        bail!("complete fixture usage is internally inconsistent");
                    }
                    counters.total_tokens = counters
                        .total_tokens
                        .checked_add(usage.total_tokens)
                        .context("fixture token total overflow")?;
                    counters.usage_complete &= usage.complete;
                } else {
                    counters.usage_complete = false;
                }
                match disposition {
                    ReplayAttemptDisposition::Completed | ReplayAttemptDisposition::Downgraded => {
                        successful_attempt = true;
                    }
                    ReplayAttemptDisposition::Retried => {}
                    ReplayAttemptDisposition::RateLimited => {
                        increment(&mut counters.errors)?;
                        rate_limit_needs_backoff = Some(counters.provider_attempts);
                    }
                    ReplayAttemptDisposition::TimedOut
                    | ReplayAttemptDisposition::TransportFailed
                    | ReplayAttemptDisposition::ProviderRejected
                    | ReplayAttemptDisposition::Cancelled => {
                        increment(&mut counters.errors)?;
                        timeout_attempt |= *disposition == ReplayAttemptDisposition::TimedOut;
                        interrupted_attempt |= *disposition == ReplayAttemptDisposition::Cancelled;
                        provider_error_attempt |= matches!(
                            disposition,
                            ReplayAttemptDisposition::TransportFailed
                                | ReplayAttemptDisposition::ProviderRejected
                        );
                    }
                }
                last_disposition = Some(*disposition);
            }
            ReplayRecord::Backoff { attempt, .. } => {
                if rate_limit_needs_backoff != Some(*attempt) {
                    bail!("backoff does not match a rate-limited attempt");
                }
                rate_limit_needs_backoff = None;
                increment(&mut counters.backoffs)?;
            }
            ReplayRecord::ToolCall {
                call_id,
                argument_shape,
                ..
            } => {
                if *call_id == 0
                    || pending_tools.contains_key(call_id)
                    || executed_tools.contains(call_id)
                {
                    bail!("tool call identity is invalid or replayed");
                }
                increment(&mut counters.tool_calls)?;
                pending_tools.insert(*call_id, (*argument_shape, None));
                if *argument_shape == ArgumentShape::MalformedJson {
                    increment(&mut counters.errors)?;
                }
            }
            ReplayRecord::PermissionRequired {
                call_id, decision, ..
            } => {
                let (_, stored_decision) = pending_tools
                    .get_mut(call_id)
                    .context("permission record has no pending tool call")?;
                if stored_decision.is_some() {
                    bail!("tool call has more than one permission decision");
                }
                *stored_decision = Some(*decision);
                increment(&mut counters.permission_requests)?;
                if *decision == PermissionDecision::Denied {
                    increment(&mut counters.errors)?;
                }
            }
            ReplayRecord::ToolResult {
                call_id, outcome, ..
            } => {
                let (shape, decision) = pending_tools
                    .remove(call_id)
                    .context("tool result has no pending tool call")?;
                if shape == ArgumentShape::MalformedJson
                    || decision == Some(PermissionDecision::Denied)
                    || !executed_tools.insert(*call_id)
                {
                    bail!("tool result would execute an invalid, denied, or duplicate call");
                }
                increment(&mut counters.tool_executions)?;
                if *outcome == ToolOutcome::Failed {
                    increment(&mut counters.errors)?;
                }
            }
            ReplayRecord::ResponseDelta {
                response_id,
                duplicate_of,
                ..
            } => {
                if *response_id == 0 {
                    bail!("response delta identity is zero");
                }
                if let Some(original) = duplicate_of {
                    if original != response_id || !deltas.contains(original) {
                        bail!("duplicate response does not reference an observed delta");
                    }
                    increment(&mut counters.duplicates_ignored)?;
                } else if deltas.insert(*response_id) {
                    increment(&mut counters.response_deltas_applied)?;
                } else {
                    bail!("response delta replay is not explicitly marked as a duplicate");
                }
            }
            ReplayRecord::DurableTerminal { category, .. } => {
                if durable_terminal.replace(*category).is_some() {
                    bail!("replay case contains more than one durable terminal record");
                }
            }
            ReplayRecord::Finish { category, .. } => {
                terminal = Some(*category);
            }
        }
    }

    let terminal = terminal.context("replay case has no terminal record")?;
    if last_disposition == Some(ReplayAttemptDisposition::Retried) {
        bail!("retried provider disposition has no following attempt");
    }
    if durable_terminal.is_some_and(|durable| durable != terminal) {
        bail!("durable terminal record disagrees with the finish category");
    }
    if rate_limit_needs_backoff.is_some() && terminal != TerminalCategory::RateLimitExhausted {
        bail!("unresolved rate limit did not end as rate_limit_exhausted");
    }
    match terminal {
        TerminalCategory::Completed
            if !pending_tools.is_empty()
                || !successful_attempt
                || !matches!(
                    last_disposition,
                    Some(
                        ReplayAttemptDisposition::Completed | ReplayAttemptDisposition::Downgraded
                    )
                ) =>
        {
            bail!("completed replay lacks successful provider evidence or leaves work pending")
        }
        TerminalCategory::ProviderError
            if !provider_error_attempt
                || !matches!(
                    last_disposition,
                    Some(
                        ReplayAttemptDisposition::ProviderRejected
                            | ReplayAttemptDisposition::TransportFailed
                    )
                ) =>
        {
            bail!("provider-error terminal lacks rejected or transport-failed evidence")
        }
        TerminalCategory::Timeout
            if !timeout_attempt && durable_terminal != Some(TerminalCategory::Timeout) =>
        {
            bail!("timeout terminal lacks timeout evidence")
        }
        TerminalCategory::Interrupted
            if !interrupted_attempt && durable_terminal != Some(TerminalCategory::Interrupted) =>
        {
            bail!("interrupted terminal lacks cancellation or durable interruption evidence")
        }
        TerminalCategory::RateLimitExhausted if rate_limit_needs_backoff.is_none() => {
            bail!("rate-limit-exhausted terminal has no unresolved final rate limit")
        }
        TerminalCategory::Failed
        | TerminalCategory::LimitReached
        | TerminalCategory::TokenCeiling
            if durable_terminal != Some(terminal) =>
        {
            bail!("durable-only terminal category has no matching durable record")
        }
        TerminalCategory::MalformedToolArguments
            if !pending_tools
                .values()
                .any(|(shape, _)| *shape == ArgumentShape::MalformedJson) =>
        {
            bail!("malformed-tool terminal has no malformed call")
        }
        TerminalCategory::PermissionDenied
            if !pending_tools
                .values()
                .any(|(_, decision)| *decision == Some(PermissionDecision::Denied)) =>
        {
            bail!("permission-denied terminal has no denied call")
        }
        _ => {}
    }

    let passed = terminal == case.expected.terminal && counters == case.expected.counters;
    Ok(ReplayCaseResult {
        id: case.id.clone(),
        terminal,
        counters,
        passed,
        diagnostic_code: if passed {
            ReplayDiagnosticCode::OracleMatch
        } else {
            ReplayDiagnosticCode::OracleMismatch
        },
    })
}

pub fn normalize_capture(capture: &PersistentAgentCapture) -> Result<ReviewCandidate> {
    capture
        .validate()
        .map_err(|_| anyhow::anyhow!("source PersistentAgentCapture is invalid"))?;
    if capture.campaign.dirty
        || !capture.provider.route_class.is_public_xai()
        || public_xai_endpoint_fingerprint(&capture.provider.route_class).as_deref()
            != Some(capture.provider.endpoint_fingerprint.as_str())
        || capture.checks.is_empty()
        || capture.checks.iter().any(|check| !check.passed)
        || !capture
            .checks
            .iter()
            .any(|check| check.name == "provider_attempts_bound_to_durable_run" && check.passed)
        || !capture
            .checks
            .iter()
            .any(|check| check.name == "provider_observation_complete" && check.passed)
        || capture.durable_states.len() != 1
        || capture.attempts.is_empty()
        || capture
            .attempts
            .iter()
            .any(|attempt| attempt.request_body.is_some() || attempt.response_body.is_some())
    {
        bail!("source capture is not eligible for structural normalization");
    }
    let durable = &capture.durable_states[0];
    // Capture v2 already requires an opaque durable Run identity. Preserve
    // that partition key exactly instead of hashing it a second time, while
    // still guaranteeing that no operational Run ID reaches the candidate.
    let source_run_id = durable.run_id.clone();
    let source_bytes = serde_json::to_vec(capture)?;
    if source_bytes.len() > MAX_FIXTURE_BYTES {
        bail!("source capture exceeds the normalizer read bound");
    }
    let source_capture_sha256 = digest(&source_bytes);
    let mut records = Vec::new();
    for attempt in &capture.attempts {
        records.push(ReplayRecord::ProviderAttempt {
            ordinal: next_ordinal(&records)?,
            disposition: map_disposition(&attempt.disposition),
            framing: map_framing(&attempt.framing),
            usage: attempt.usage.as_ref().map(|usage| UsageShape {
                input_tokens: usage.prompt_tokens,
                output_tokens: usage.completion_tokens,
                total_tokens: usage.total_tokens,
                complete: usage.complete,
            }),
        });
    }
    let terminal = capture_terminal(capture)?;
    if !capture.durable_states.is_empty() {
        records.push(ReplayRecord::DurableTerminal {
            ordinal: next_ordinal(&records)?,
            category: terminal,
        });
    }
    records.push(ReplayRecord::Finish {
        ordinal: next_ordinal(&records)?,
        category: terminal,
    });
    let expected_counters = counters_for_records(&records)?;
    let case_id = format!("normalized-{}", capture.campaign.scenario_id);
    let fixture = ReplayFixture {
        schema: REPLAY_FIXTURE_SCHEMA.into(),
        fixture_id: format!(
            "candidate-{}-{}",
            capture.campaign.scenario_id,
            &source_capture_sha256[..12]
        ),
        kind: ReplayFixtureKind::LiveCaptureStructural,
        cases: vec![ReplayCase {
            id: case_id,
            catalog_scenario_id: capture.campaign.scenario_id.clone(),
            source_run_id: Some(source_run_id.clone()),
            records,
            expected: ReplayExpected {
                terminal,
                counters: expected_counters,
            },
        }],
    };
    let replay = replay_fixture(&fixture)?;
    if !replay.passed {
        bail!("normalized candidate failed its own deterministic replay oracle");
    }
    let candidate = ReviewCandidate {
        schema: REVIEW_CANDIDATE_SCHEMA.into(),
        candidate_id: fixture.fixture_id.clone(),
        source_capture_schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
        fixture,
        promotion_manifest: PromotionManifest {
            source_capture_sha256,
            source_campaign_id: capture.campaign.campaign_id.clone(),
            source_run_ids: vec![source_run_id],
            catalog_scenario_id: capture.campaign.scenario_id.clone(),
            normalizer_version: NORMALIZER_VERSION.into(),
            retained_fields: strings(RETAINED_FIELDS),
            transformations: strings(TRANSFORMATIONS),
            omissions: strings(OMISSIONS),
            fixture_sha256: replay.fixture_sha256,
            oracle_sha256: replay.oracle_sha256,
            automatic_promotion: false,
            human_review_required: true,
            structural_only: true,
            fixture_promotion_eligible: false,
        },
    };
    validate_candidate(&candidate)?;
    Ok(candidate)
}

pub fn validate_candidate(candidate: &ReviewCandidate) -> Result<Vec<u8>> {
    if candidate.schema != REVIEW_CANDIDATE_SCHEMA
        || candidate.source_capture_schema != PERSISTENT_AGENT_CAPTURE_SCHEMA
        || candidate.fixture.kind != ReplayFixtureKind::LiveCaptureStructural
        || candidate.fixture.cases.len() != 1
        || candidate.candidate_id != candidate.fixture.fixture_id
        || candidate.promotion_manifest.automatic_promotion
        || !candidate.promotion_manifest.human_review_required
        || !candidate.promotion_manifest.structural_only
        || candidate.promotion_manifest.fixture_promotion_eligible
        || candidate.promotion_manifest.normalizer_version != NORMALIZER_VERSION
        || candidate.promotion_manifest.retained_fields != strings(RETAINED_FIELDS)
        || candidate.promotion_manifest.transformations != strings(TRANSFORMATIONS)
        || candidate.promotion_manifest.omissions != strings(OMISSIONS)
    {
        bail!("review candidate violates the promotion boundary");
    }
    validate_safe_id(&candidate.candidate_id)?;
    validate_hash(&candidate.promotion_manifest.source_capture_sha256)?;
    validate_opaque_label(&candidate.promotion_manifest.source_campaign_id)?;
    validate_hash(&candidate.promotion_manifest.fixture_sha256)?;
    validate_hash(&candidate.promotion_manifest.oracle_sha256)?;
    if !PERSISTENT_AGENT_SCENARIO_IDS
        .contains(&candidate.promotion_manifest.catalog_scenario_id.as_str())
    {
        bail!("review candidate references an unknown catalog scenario");
    }
    let replay = replay_fixture(&candidate.fixture)?;
    if replay.fixture_sha256 != candidate.promotion_manifest.fixture_sha256
        || replay.oracle_sha256 != candidate.promotion_manifest.oracle_sha256
        || candidate.promotion_manifest.catalog_scenario_id
            != candidate.fixture.cases[0].catalog_scenario_id
        || candidate.promotion_manifest.source_run_ids.len() != 1
        || candidate.promotion_manifest.source_run_ids[0]
            != candidate.fixture.cases[0]
                .source_run_id
                .as_deref()
                .context("live structural case has no Run partition")?
    {
        bail!("review candidate manifest does not match its fixture");
    }
    let value = serde_json::to_value(candidate)?;
    scan_value_for_forbidden_data(&value)
        .map_err(|_| anyhow::anyhow!("review candidate failed forbidden-data scanning"))?;
    let bytes = serde_json::to_vec_pretty(candidate)?;
    if bytes.len() > MAX_FIXTURE_BYTES {
        bail!("review candidate exceeds the byte bound");
    }
    Ok(bytes)
}

fn counters_for_records(records: &[ReplayRecord]) -> Result<ReplayCounters> {
    let case = ReplayCase {
        id: "counter-projection".into(),
        catalog_scenario_id: "sse-stream-001".into(),
        source_run_id: Some(format!("opaque-{}", "0".repeat(64))),
        records: records.to_vec(),
        expected: ReplayExpected {
            terminal: records
                .iter()
                .find_map(|record| match record {
                    ReplayRecord::Finish { category, .. } => Some(*category),
                    _ => None,
                })
                .context("records have no terminal category")?,
            counters: ReplayCounters::default(),
        },
    };
    Ok(replay_case(ReplayFixtureKind::LiveCaptureStructural, &case)?.counters)
}

fn validate_attempt_framing(
    disposition: ReplayAttemptDisposition,
    framing: ReplayFraming,
) -> Result<()> {
    if disposition == ReplayAttemptDisposition::TransportFailed
        && framing != ReplayFraming::TransportFailure
    {
        bail!("transport-failed disposition lacks transport-failure framing");
    }
    if framing == ReplayFraming::TransportFailure
        && disposition != ReplayAttemptDisposition::TransportFailed
    {
        bail!("transport-failure framing is inconsistent with attempt disposition");
    }
    if matches!(
        disposition,
        ReplayAttemptDisposition::TimedOut | ReplayAttemptDisposition::Cancelled
    ) && framing != ReplayFraming::NoBody
    {
        bail!("timeout or cancellation disposition must use no-body framing");
    }
    Ok(())
}

fn capture_terminal(capture: &PersistentAgentCapture) -> Result<TerminalCategory> {
    if let Some(state) = capture.durable_states.last() {
        return match state.terminal_state.as_str() {
            "completed" | "succeeded" => Ok(TerminalCategory::Completed),
            "interrupted" | "cancelled" => Ok(TerminalCategory::Interrupted),
            "failed"
                if capture.attempts.last().is_some_and(|attempt| {
                    matches!(
                        attempt.disposition,
                        CaptureAttemptDisposition::ProviderRejected
                            | CaptureAttemptDisposition::TransportFailed
                    )
                }) =>
            {
                Ok(TerminalCategory::ProviderError)
            }
            "failed" => Ok(TerminalCategory::Failed),
            "limit_reached" => match state.stop_cause.as_deref() {
                Some("duration_limit") => Ok(TerminalCategory::Timeout),
                Some("token_ceiling") => Ok(TerminalCategory::TokenCeiling),
                Some("round_limit" | "stationarity" | "recovery_exhausted") => {
                    Ok(TerminalCategory::LimitReached)
                }
                _ => bail!("capture limit state has ambiguous stop evidence"),
            },
            _ => bail!("capture has an unsupported terminal state"),
        };
    }
    match capture.attempts.last().map(|attempt| &attempt.disposition) {
        Some(CaptureAttemptDisposition::Success | CaptureAttemptDisposition::Downgraded) => {
            Ok(TerminalCategory::Completed)
        }
        Some(CaptureAttemptDisposition::RateLimited) => Ok(TerminalCategory::RateLimitExhausted),
        Some(CaptureAttemptDisposition::TimedOut) => Ok(TerminalCategory::Timeout),
        Some(CaptureAttemptDisposition::Cancelled) => Ok(TerminalCategory::Interrupted),
        Some(
            CaptureAttemptDisposition::Retried
            | CaptureAttemptDisposition::TransportFailed
            | CaptureAttemptDisposition::ProviderRejected,
        ) => Ok(TerminalCategory::ProviderError),
        None => bail!("capture has neither durable state nor provider attempt evidence"),
    }
}

fn map_disposition(value: &CaptureAttemptDisposition) -> ReplayAttemptDisposition {
    match value {
        CaptureAttemptDisposition::Success => ReplayAttemptDisposition::Completed,
        CaptureAttemptDisposition::Retried => ReplayAttemptDisposition::Retried,
        CaptureAttemptDisposition::Downgraded => ReplayAttemptDisposition::Downgraded,
        CaptureAttemptDisposition::RateLimited => ReplayAttemptDisposition::RateLimited,
        CaptureAttemptDisposition::TimedOut => ReplayAttemptDisposition::TimedOut,
        CaptureAttemptDisposition::TransportFailed => ReplayAttemptDisposition::TransportFailed,
        CaptureAttemptDisposition::ProviderRejected => ReplayAttemptDisposition::ProviderRejected,
        CaptureAttemptDisposition::Cancelled => ReplayAttemptDisposition::Cancelled,
    }
}

fn map_framing(value: &CaptureFraming) -> ReplayFraming {
    match value {
        CaptureFraming::Sse => ReplayFraming::Sse,
        CaptureFraming::Json => ReplayFraming::Json,
        CaptureFraming::NoBody => ReplayFraming::NoBody,
        CaptureFraming::TransportFailure => ReplayFraming::TransportFailure,
    }
}

fn next_ordinal(records: &[ReplayRecord]) -> Result<u32> {
    let next = records
        .len()
        .checked_add(1)
        .context("replay record ordinal overflow")?;
    u32::try_from(next).context("replay record ordinal exceeds u32")
}

fn increment(value: &mut u32) -> Result<()> {
    *value = value.checked_add(1).context("replay counter overflow")?;
    if *value as usize > MAX_RECORDS_PER_CASE {
        bail!("replay counter exceeds the record bound");
    }
    Ok(())
}

fn validate_replay_counters(counters: &ReplayCounters) -> Result<()> {
    for value in [
        counters.provider_attempts,
        counters.tool_calls,
        counters.tool_executions,
        counters.permission_requests,
        counters.response_deltas_applied,
        counters.duplicates_ignored,
        counters.backoffs,
        counters.errors,
    ] {
        if value as usize > MAX_RECORDS_PER_CASE {
            bail!("replay counter exceeds the record bound");
        }
    }
    if counters.total_tokens > MAX_REPLAY_TOKENS {
        bail!("replay token counter exceeds the campaign bound");
    }
    Ok(())
}

fn validate_hash(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("review candidate hash is not lowercase SHA-256");
    }
    Ok(())
}

fn validate_opaque_label(value: &str) -> Result<()> {
    if value.len() != 71
        || !value.starts_with("opaque-")
        || !value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("review candidate opaque identity is invalid");
    }
    Ok(())
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use grokptah_agent_bridge::{
        public_xai_endpoint_fingerprint, AttemptDisposition, CampaignActuals, CampaignBudgets,
        CampaignIdentity, CertificationBoundProfile, CertificationCheck, CredentialMethodClass,
        DurableStateEvidence, ProviderAttemptEvidence, ProviderDialectClass, ProviderIdentity,
        ProviderRouteClass, StreamFraming, UsageEvidence,
    };

    fn normalizable_capture() -> PersistentAgentCapture {
        PersistentAgentCapture {
            schema: PERSISTENT_AGENT_CAPTURE_SCHEMA.into(),
            campaign: CampaignIdentity {
                campaign_id: format!("opaque-{}", "9".repeat(64)),
                scenario_id: "sse-stream-001".into(),
                repository_commit: "8ea4b779".into(),
                dirty: false,
            },
            provider: ProviderIdentity {
                route_class: ProviderRouteClass::GrokBuildProxy,
                dialect: ProviderDialectClass::XaiChatCompletions,
                credential_method: CredentialMethodClass::GrokBuildOidc,
                model_identity: "grok-build".into(),
                endpoint_fingerprint: public_xai_endpoint_fingerprint(
                    &ProviderRouteClass::GrokBuildProxy,
                )
                .unwrap(),
            },
            budgets: CampaignBudgets {
                bound_profile: CertificationBoundProfile::Smoke,
                max_run_tokens: 1_000,
                max_total_tokens: 1_000,
                max_provider_requests: 4,
                max_continuations: 1,
                max_duration_seconds: 60,
                max_raw_artifact_bytes: 1_024,
                max_response_bytes_per_request: 1_024,
            },
            actuals: CampaignActuals {
                total_tokens: 3,
                provider_requests: 1,
                continuations: 0,
                duration_seconds: 1,
                raw_artifact_bytes: 0,
            },
            attempts: vec![ProviderAttemptEvidence {
                attempt: 1,
                method: "POST".into(),
                route_identity: "/v1/chat/completions".into(),
                present_request_headers: vec!["content-type".into()],
                request_body: None,
                response_body: None,
                response_status: Some(200),
                response_content_type: Some("text/event-stream".into()),
                framing: StreamFraming::Sse,
                disposition: AttemptDisposition::Success,
                usage: Some(UsageEvidence {
                    prompt_tokens: 2,
                    completion_tokens: 1,
                    total_tokens: 3,
                    complete: true,
                }),
                latency_millis: 1,
            }],
            durable_states: vec![DurableStateEvidence {
                agent_id: format!("opaque-{}", "1".repeat(64)),
                agent_spec_revision: 1,
                lane_id: format!("opaque-{}", "2".repeat(64)),
                run_id: format!("opaque-{}", "3".repeat(64)),
                parent_run_id: None,
                checkpoint_id: None,
                continuation_input_hash: None,
                continuation_context_hash: None,
                continuation_fidelity: None,
                terminal_state: "completed".into(),
                stop_cause: None,
            }],
            checks: vec![
                CertificationCheck {
                    name: "provider_attempts_bound_to_durable_run".into(),
                    passed: true,
                    detail_code: "scope-match".into(),
                },
                CertificationCheck {
                    name: "provider_observation_complete".into(),
                    passed: true,
                    detail_code: "recorder-complete".into(),
                },
            ],
        }
    }

    #[test]
    fn representative_fixture_replays_deterministically() {
        let text = include_str!("../replay-fixtures/provider-behaviors.v1.json");
        let fixture: ReplayFixture = serde_json::from_str(text).unwrap();
        let first = replay_fixture(&fixture).unwrap();
        let second = replay_fixture(&fixture).unwrap();
        assert!(first.passed);
        assert_eq!(first, second);
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn malformed_unknown_and_oversized_fixtures_fail_closed() {
        assert!(serde_json::from_str::<ReplayFixture>("{not-json").is_err());
        assert!(serde_json::from_str::<ReplayFixture>(
            r#"{"schema":"unknown","fixture_id":"x","cases":[],"extra":true}"#
        )
        .is_err());
        let fixture = ReplayFixture {
            schema: "unknown".into(),
            fixture_id: "fixture".into(),
            kind: ReplayFixtureKind::SyntheticBehavior,
            cases: vec![],
        };
        assert!(replay_fixture(&fixture).is_err());
    }

    #[test]
    fn duplicate_delta_must_be_explicit_and_tool_calls_are_exactly_once() {
        let records = vec![
            ReplayRecord::ProviderAttempt {
                ordinal: 1,
                disposition: ReplayAttemptDisposition::Completed,
                framing: ReplayFraming::Sse,
                usage: Some(UsageShape {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                    complete: true,
                }),
            },
            ReplayRecord::ResponseDelta {
                ordinal: 2,
                response_id: 1,
                duplicate_of: None,
            },
            ReplayRecord::ResponseDelta {
                ordinal: 3,
                response_id: 1,
                duplicate_of: None,
            },
            ReplayRecord::Finish {
                ordinal: 4,
                category: TerminalCategory::Completed,
            },
        ];
        let case = ReplayCase {
            id: "bad-duplicate".into(),
            catalog_scenario_id: "sse-stream-001".into(),
            source_run_id: None,
            records,
            expected: ReplayExpected {
                terminal: TerminalCategory::Completed,
                counters: ReplayCounters {
                    provider_attempts: 1,
                    tool_calls: 0,
                    tool_executions: 0,
                    permission_requests: 0,
                    response_deltas_applied: 1,
                    duplicates_ignored: 0,
                    backoffs: 0,
                    errors: 0,
                    total_tokens: 2,
                    usage_complete: true,
                },
            },
        };
        assert!(replay_case(ReplayFixtureKind::SyntheticBehavior, &case).is_err());
    }

    #[test]
    fn malformed_utf8_is_rejected_before_fixture_parsing() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0xf0, 0x9f, 0x92]);
        assert!(std::str::from_utf8(&bytes).is_err());
    }

    #[test]
    fn live_normalization_binds_campaign_and_run_and_is_never_auto_promotable() {
        let candidate = normalize_capture(&normalizable_capture()).unwrap();
        assert!(candidate.promotion_manifest.structural_only);
        assert!(!candidate.promotion_manifest.fixture_promotion_eligible);
        assert!(!candidate.promotion_manifest.automatic_promotion);
        assert_eq!(candidate.promotion_manifest.source_run_ids.len(), 1);
        assert_eq!(
            candidate.fixture.cases[0].source_run_id.as_ref(),
            candidate.promotion_manifest.source_run_ids.first()
        );
        validate_candidate(&candidate).unwrap();

        let mut mismatched = candidate;
        mismatched.promotion_manifest.source_run_ids[0] = format!("opaque-{}", "8".repeat(64));
        assert!(validate_candidate(&mismatched).is_err());
    }

    #[test]
    fn dirty_failed_private_or_run_ambiguous_captures_are_not_normalized() {
        let mut capture = normalizable_capture();
        capture.campaign.dirty = true;
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        capture.checks[0].passed = false;
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        capture.checks.clear();
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        capture
            .checks
            .retain(|check| check.name != "provider_observation_complete");
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        capture
            .durable_states
            .push(capture.durable_states[0].clone());
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        let mut contradictory = capture.attempts[0].clone();
        contradictory.attempt = 2;
        contradictory.disposition = CaptureAttemptDisposition::ProviderRejected;
        contradictory.response_status = Some(400);
        contradictory.response_content_type = Some("application/json".into());
        contradictory.framing = CaptureFraming::Json;
        capture.attempts.push(contradictory);
        capture.actuals.provider_requests = 2;
        capture.actuals.total_tokens = 6;
        assert!(capture.validate().is_ok());
        assert!(normalize_capture(&capture).is_err());

        let mut capture = normalizable_capture();
        capture.provider.route_class = ProviderRouteClass::CompatibleGateway;
        capture.provider.dialect = ProviderDialectClass::OpenAiCompatible;
        capture.provider.credential_method = CredentialMethodClass::ManagedProviderReference;
        capture.provider.model_identity = format!("opaque-{}", "7".repeat(64));
        capture.provider.endpoint_fingerprint = "6".repeat(64);
        capture.attempts[0].route_identity = format!("opaque-{}", "5".repeat(64));
        assert!(capture.validate().is_ok());
        assert!(normalize_capture(&capture).is_err());
    }
}
