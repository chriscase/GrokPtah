//! Exact-identity evidence for the always-on Grokbot certification probe.
//!
//! The probe used to compare bare cardinalities (`work`/`runs`/`intents`
//! counts). Counts are cardinality-neutral: swapping one durable identity for
//! another leaves them untouched, so a replayed request that silently rebuilt
//! the Work set, or a restart that resurrected a run under a fresh id, passed.
//! This module replaces that oracle with a deterministic, fully ordered
//! identity snapshot and keeps the counts as a redundant cross-check.
//!
//! Everything here observes only the public MCP projection: `ptah_list_work`,
//! `ptah_get_work`, `ptah_list_execution_intents` and `ptah_list_runs`. No
//! product crate is modified and no private state is read.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::report::{
    opaque_durable_id, DiagnosticCode, LoopbackProviderObservation, LoopbackProviderRecord,
};

/// Reserved public step id under which the manager-decision lane is projected
/// by `ptah_list_work.sourceManagerStepId`.
pub const MANAGER_DECISION_STEP_ID: &str = "__manager_decision__";
/// Public `kind` discriminator for manager-decision Work.
pub const MANAGER_DECISION_KIND: &str = "manager-decision";
/// Public `kind` of the Work row the manager plan itself occupies. It carries
/// no step id and no attempts, and is observed in `ptah_list_work` alongside
/// the step lanes.
pub const MANAGER_PLAN_KIND: &str = "manager-plan";
/// Work rows one manager plan contributes on top of its step lanes.
const MANAGER_PLAN_WORK_ROWS: usize = 1;
/// Public `purpose` discriminator for the manager proposal Run.
pub const MANAGER_PROPOSAL_PURPOSE: &str = "manager_proposal";
/// Loopback provider semantic id for the bootstrap submit.
pub const SETUP_SEMANTIC_ID: &str = "setup";
/// Semantic ids that may never be reused as manager step ids.
const RESERVED_SEMANTIC_IDS: &[&str] = &[
    MANAGER_DECISION_KIND,
    MANAGER_DECISION_STEP_ID,
    SETUP_SEMANTIC_ID,
];

fn invalid(_: impl fmt::Display) -> DiagnosticCode {
    DiagnosticCode::FixtureInvalid
}

// ---------------------------------------------------------------------------
// Duplicate-rejecting JSON
// ---------------------------------------------------------------------------

/// A `serde_json::Value` that refuses duplicate object keys at any depth.
///
/// `serde_json` silently keeps the last binding for a repeated key, so a
/// fixture carrying `"decisionWork": 1, "decisionWork": 9` would parse as `9`
/// with no diagnostic. Certification fixtures must fail closed instead.
struct DuplicateFree(Value);

impl<'de> Deserialize<'de> for DuplicateFree {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(DuplicateFreeVisitor).map(Self)
    }
}

struct DuplicateFreeVisitor;

impl<'de> Visitor<'de> for DuplicateFreeVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object keys")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Ok(Value::from(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(DuplicateFree(item)) = access.next_element()? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            let DuplicateFree(value) = access.next_value()?;
            if map.insert(key.clone(), value).is_some() {
                return Err(de::Error::custom(format!("duplicate object key {key}")));
            }
        }
        Ok(Value::Object(map))
    }
}

/// Parse `bytes` as JSON, rejecting duplicate object keys at any depth.
pub fn parse_duplicate_free(bytes: &[u8]) -> Result<Value, String> {
    serde_json::from_slice::<DuplicateFree>(bytes)
        .map(|DuplicateFree(value)| value)
        .map_err(|error| format!("fixture JSON: {error}"))
}

// ---------------------------------------------------------------------------
// Typed, exact-key fixture
// ---------------------------------------------------------------------------

/// One `failClosed` row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailClosedExpect {
    pub run_state: String,
    pub stop_cause: String,
    pub error_code: String,
    pub posts: u64,
}

/// Bounded resource ceilings declared by the fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResourceCeilings {
    pub max_rss_bytes: u64,
    pub max_fd_count: u64,
    pub max_threads: u64,
    pub max_disk_bytes: u64,
    pub max_cycle_latency_ms: u64,
    pub max_rss_growth_bytes: u64,
    pub max_fd_growth: u64,
    pub max_thread_growth: u64,
    pub max_disk_growth_bytes: u64,
}

/// Bounded artifact-scan budget declared by the fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArtifactScan {
    pub max_depth: u64,
    pub max_files: u64,
    pub max_file_bytes: u64,
    pub stderr_head_bytes: u64,
    pub stderr_tail_bytes: u64,
}

/// The always-on fixture, parsed with an exhaustive exact-key validator.
///
/// Every key of the document is consumed by name; anything left over is a hard
/// error. That makes the fixture the single source of truth for the runtime
/// oracle: a key the lab does not understand cannot be smuggled in, and a key
/// the lab needs cannot be silently dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AlwaysOnFixture {
    pub schema: String,
    pub schema_version: u64,
    pub seed: String,
    pub base_sha: String,
    pub claim: String,
    pub next_required_campaign: String,
    pub quota_ledger: String,
    pub proposal_only_enforcement: String,
    pub internal_persistence_cuts: String,
    pub attempt_evidence: String,
    pub provider_attempt_projection: String,
    pub uncertain_accept_projection: String,
    pub retry_class_projection: String,
    pub clock: String,
    pub proved_oracle: String,
    pub soak10m: String,
    pub soak24h: String,
    pub ci_mode: String,
    pub sentinel_success: String,
    pub sentinel_fail: String,
    pub sentinel_ok: String,
    pub sentinel_setup: String,
    pub step_first: String,
    pub step_failing: String,
    pub step_replacement: String,
    pub decision_work: u64,
    pub proposal_runs: u64,
    pub native_work_by_step: BTreeMap<String, u64>,
    pub provider_posts_by_semantic: BTreeMap<String, u64>,
    pub fail_closed: BTreeMap<String, FailClosedExpect>,
    pub ceilings: ResourceCeilings,
    pub artifact_scan: ArtifactScan,
    pub required_assertions: Vec<String>,
    pub supervisor_period: Duration,
    pub zero_growth_periods: u64,
    pub zero_growth_window: Duration,
}

impl AlwaysOnFixture {
    /// Parse the fixture bundled with the lab.
    pub fn load() -> Result<Self, DiagnosticCode> {
        Self::parse(crate::ALWAYS_ON_GROKBOT_FIXTURE).map_err(invalid)
    }

    /// Parse `bytes` with the strict validator, reporting the first violation.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        Self::from_value(parse_duplicate_free(bytes)?)
    }

    /// Back-compat entry point for callers holding an already-parsed document.
    ///
    /// Prefer [`AlwaysOnFixture::parse`]: a `Value` has already lost duplicate
    /// keys, so this path cannot enforce the no-duplicates rule.
    pub fn from_json(value: &Value) -> Result<Self, DiagnosticCode> {
        Self::from_value(value.clone()).map_err(invalid)
    }

    fn from_value(value: Value) -> Result<Self, String> {
        let mut root = expect_object(value, "fixture")?;
        let schema = take_string(&mut root, "schema")?;
        if schema != crate::ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA {
            return Err(format!(
                "schema {schema} != {}",
                crate::ALWAYS_ON_GROKBOT_FIXTURE_SCHEMA
            ));
        }
        let schema_version = take_u64(&mut root, "schemaVersion")?;
        if schema_version != 2 {
            return Err(format!("schemaVersion {schema_version} != 2"));
        }
        let seed = take_string(&mut root, "seed")?;
        let base_sha = take_string(&mut root, "baseSha")?;
        let claim = take_string(&mut root, "claim")?;
        let next_required_campaign = take_string(&mut root, "nextRequiredCampaign")?;
        let quota_ledger = take_string(&mut root, "quotaLedger")?;
        let proposal_only_enforcement = take_string(&mut root, "proposalOnlyEnforcement")?;
        let internal_persistence_cuts = take_string(&mut root, "internalPersistenceCuts")?;
        let attempt_evidence = take_string(&mut root, "attemptEvidence")?;
        let provider_attempt_projection = take_string(&mut root, "providerAttemptProjection")?;
        let uncertain_accept_projection = take_string(&mut root, "uncertainAcceptProjection")?;
        let retry_class_projection = take_string(&mut root, "retryClassProjection")?;
        let clock = take_string(&mut root, "clock")?;
        let supervisor_period_ms = take_u64(&mut root, "supervisorPeriodMs")?;
        let zero_growth_periods = take_u64(&mut root, "zeroGrowthSupervisorPeriods")?;
        let proved_oracle = take_string(&mut root, "provedOracle")?;
        let soak10m = take_string(&mut root, "soak10m")?;
        let soak24h = take_string(&mut root, "soak24h")?;
        let ci_mode = take_string(&mut root, "ciMode")?;

        let mut sentinels = take_object(&mut root, "sentinels")?;
        let sentinel_success = take_string(&mut sentinels, "success")?;
        let sentinel_fail = take_string(&mut sentinels, "fail")?;
        let sentinel_ok = take_string(&mut sentinels, "ok")?;
        let sentinel_setup = take_string(&mut sentinels, "setup")?;
        deny_unknown(sentinels, "sentinels")?;

        let mut steps = take_object(&mut root, "steps")?;
        let step_first = take_string(&mut steps, "first")?;
        let step_failing = take_string(&mut steps, "failing")?;
        let step_replacement = take_string(&mut steps, "replacement")?;
        deny_unknown(steps, "steps")?;

        let mut happy = take_object(&mut root, "happyPath")?;
        let decision_work = take_u64(&mut happy, "decisionWork")?;
        let proposal_runs = take_u64(&mut happy, "proposalRunsObserved")?;
        let native_work_by_step = take_u64_map(&mut happy, "nativeWorkByStep")?;
        let provider_posts_by_semantic = take_u64_map(&mut happy, "providerPostsBySemanticId")?;
        deny_unknown(happy, "happyPath")?;

        let fail_closed = take_fail_closed(&mut root)?;
        let ceilings = take_ceilings(&mut root)?;
        let artifact_scan = take_artifact_scan(&mut root)?;
        let required_assertions = take_string_array(&mut root, "requiredAssertions")?;
        deny_unknown(root, "fixture")?;

        let fixture = Self {
            schema,
            schema_version,
            seed,
            base_sha,
            claim,
            next_required_campaign,
            quota_ledger,
            proposal_only_enforcement,
            internal_persistence_cuts,
            attempt_evidence,
            provider_attempt_projection,
            uncertain_accept_projection,
            retry_class_projection,
            clock,
            proved_oracle,
            soak10m,
            soak24h,
            ci_mode,
            sentinel_success,
            sentinel_fail,
            sentinel_ok,
            sentinel_setup,
            step_first,
            step_failing,
            step_replacement,
            decision_work,
            proposal_runs,
            native_work_by_step,
            provider_posts_by_semantic,
            fail_closed,
            ceilings,
            artifact_scan,
            required_assertions,
            supervisor_period: Duration::from_millis(supervisor_period_ms),
            zero_growth_periods,
            zero_growth_window: Duration::from_millis(
                supervisor_period_ms
                    .checked_mul(zero_growth_periods)
                    .ok_or("supervisorPeriodMs * zeroGrowthSupervisorPeriods overflows")?,
            ),
        };
        fixture.validate(supervisor_period_ms)?;
        Ok(fixture)
    }

    fn validate(&self, supervisor_period_ms: u64) -> Result<(), String> {
        if supervisor_period_ms == 0 {
            return Err("supervisorPeriodMs must be greater than zero".into());
        }
        if self.zero_growth_periods == 0 {
            return Err("zeroGrowthSupervisorPeriods must be greater than zero".into());
        }
        if self.required_assertions.len()
            != self
                .required_assertions
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
        {
            return Err("requiredAssertions must be unique".into());
        }
        let steps = self.native_steps();
        if steps.iter().collect::<BTreeSet<_>>().len() != steps.len() {
            return Err("steps.first/failing/replacement must be distinct".into());
        }
        for step in &steps {
            if RESERVED_SEMANTIC_IDS.contains(&step.as_str()) {
                return Err(format!(
                    "step id {step} collides with a reserved semantic id"
                ));
            }
        }
        let expected_native: BTreeSet<&String> = steps.iter().collect();
        let declared_native: BTreeSet<&String> = self.native_work_by_step.keys().collect();
        if declared_native != expected_native {
            return Err("happyPath.nativeWorkByStep keys must be exactly the three steps".into());
        }
        let mut expected_posts: BTreeSet<String> = steps.iter().cloned().collect();
        expected_posts.insert(MANAGER_DECISION_KIND.to_owned());
        let declared_posts: BTreeSet<String> =
            self.provider_posts_by_semantic.keys().cloned().collect();
        if declared_posts != expected_posts {
            return Err(
                "happyPath.providerPostsBySemanticId keys must be exactly the three steps plus manager-decision"
                    .into(),
            );
        }
        for step in &steps {
            if self.native_work_by_step.get(step) != Some(&1) {
                return Err(format!(
                    "happyPath.nativeWorkByStep.{step} must be exactly 1"
                ));
            }
            if self.provider_posts_by_semantic.get(step) != Some(&1) {
                return Err(format!(
                    "happyPath.providerPostsBySemanticId.{step} must be exactly 1"
                ));
            }
        }
        if self.provider_posts_by_semantic.get(MANAGER_DECISION_KIND) != Some(&1) {
            return Err("happyPath.providerPostsBySemanticId.manager-decision must be 1".into());
        }
        if self.decision_work != 1 {
            return Err("happyPath.decisionWork must be exactly 1".into());
        }
        if self.proposal_runs != 1 {
            return Err("happyPath.proposalRunsObserved must be exactly 1".into());
        }
        Ok(())
    }

    /// The three fixture-declared native step ids, in plan order.
    pub fn native_steps(&self) -> [String; 3] {
        [
            self.step_first.clone(),
            self.step_failing.clone(),
            self.step_replacement.clone(),
        ]
    }

    /// Provider POSTs the fixture expects for `semantic_id`.
    pub fn posts_for(&self, semantic_id: &str) -> Option<u64> {
        self.provider_posts_by_semantic.get(semantic_id).copied()
    }

    /// Total provider POSTs across every declared semantic id plus the single
    /// bootstrap `setup` POST, which is not a plan step.
    pub fn expected_total_posts(&self) -> Result<u64, DiagnosticCode> {
        self.provider_posts_by_semantic
            .values()
            .try_fold(1_u64, |total, count| total.checked_add(*count))
            .ok_or(DiagnosticCode::FixtureInvalid)
    }
}

fn expect_object(value: Value, ctx: &str) -> Result<Map<String, Value>, String> {
    match value {
        Value::Object(map) => Ok(map),
        other => Err(format!("{ctx} must be an object, got {other}")),
    }
}

fn deny_unknown(map: Map<String, Value>, ctx: &str) -> Result<(), String> {
    if map.is_empty() {
        return Ok(());
    }
    let unknown: Vec<&String> = map.keys().collect();
    Err(format!("{ctx} has unknown keys {unknown:?}"))
}

fn take_string(map: &mut Map<String, Value>, key: &str) -> Result<String, String> {
    match map.remove(key) {
        Some(Value::String(value)) if !value.is_empty() => Ok(value),
        Some(other) => Err(format!("{key} must be a non-empty string, got {other}")),
        None => Err(format!("missing {key}")),
    }
}

fn take_u64(map: &mut Map<String, Value>, key: &str) -> Result<u64, String> {
    match map.remove(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .ok_or_else(|| format!("{key} must be a u64")),
        Some(other) => Err(format!("{key} must be a u64, got {other}")),
        None => Err(format!("missing {key}")),
    }
}

fn take_object(map: &mut Map<String, Value>, key: &str) -> Result<Map<String, Value>, String> {
    expect_object(
        map.remove(key).ok_or_else(|| format!("missing {key}"))?,
        key,
    )
}

fn take_u64_map(map: &mut Map<String, Value>, key: &str) -> Result<BTreeMap<String, u64>, String> {
    let object = take_object(map, key)?;
    let mut out = BTreeMap::new();
    for (name, value) in object {
        if name.is_empty() {
            return Err(format!("{key} has an empty key"));
        }
        let count = value
            .as_u64()
            .ok_or_else(|| format!("{key}.{name} must be a u64"))?;
        if out.insert(name.clone(), count).is_some() {
            return Err(format!("duplicate {key}.{name}"));
        }
    }
    Ok(out)
}

fn take_string_array(map: &mut Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match map.remove(key) {
        Some(Value::Array(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(value) if !value.is_empty() => Ok(value),
                other => Err(format!(
                    "{key} item must be a non-empty string, got {other}"
                )),
            })
            .collect(),
        Some(other) => Err(format!("{key} must be an array, got {other}")),
        None => Err(format!("missing {key}")),
    }
}

fn take_fail_closed(
    root: &mut Map<String, Value>,
) -> Result<BTreeMap<String, FailClosedExpect>, String> {
    let object = take_object(root, "failClosed")?;
    let mut out = BTreeMap::new();
    for (name, value) in object {
        let mut row = expect_object(value, &format!("failClosed.{name}"))?;
        let expect = FailClosedExpect {
            run_state: take_string(&mut row, "runState")?,
            stop_cause: take_string(&mut row, "stopCause")?,
            error_code: take_string(&mut row, "errorCode")?,
            posts: take_u64(&mut row, "posts")?,
        };
        deny_unknown(row, &format!("failClosed.{name}"))?;
        if expect.posts != 1 {
            return Err(format!("failClosed.{name}.posts must be exactly 1"));
        }
        out.insert(name, expect);
    }
    let required: BTreeSet<&str> = ["cancel", "malformed", "disconnect", "status500", "slow"]
        .into_iter()
        .collect();
    let declared: BTreeSet<&str> = out.keys().map(String::as_str).collect();
    if declared != required {
        return Err("failClosed must declare exactly the five required cases".into());
    }
    Ok(out)
}

fn take_ceilings(root: &mut Map<String, Value>) -> Result<ResourceCeilings, String> {
    let mut map = take_object(root, "resourceCeilings")?;
    let ceilings = ResourceCeilings {
        max_rss_bytes: take_u64(&mut map, "maxRssBytes")?,
        max_fd_count: take_u64(&mut map, "maxFdCount")?,
        max_threads: take_u64(&mut map, "maxThreads")?,
        max_disk_bytes: take_u64(&mut map, "maxDiskBytes")?,
        max_cycle_latency_ms: take_u64(&mut map, "maxCycleLatencyMs")?,
        max_rss_growth_bytes: take_u64(&mut map, "maxRssGrowthBytes")?,
        max_fd_growth: take_u64(&mut map, "maxFdGrowth")?,
        max_thread_growth: take_u64(&mut map, "maxThreadGrowth")?,
        max_disk_growth_bytes: take_u64(&mut map, "maxDiskGrowthBytes")?,
    };
    deny_unknown(map, "resourceCeilings")?;
    for (name, value) in [
        ("maxRssBytes", ceilings.max_rss_bytes),
        ("maxFdCount", ceilings.max_fd_count),
        ("maxThreads", ceilings.max_threads),
        ("maxDiskBytes", ceilings.max_disk_bytes),
        ("maxCycleLatencyMs", ceilings.max_cycle_latency_ms),
    ] {
        if value == 0 {
            return Err(format!("resourceCeilings.{name} must be greater than zero"));
        }
    }
    Ok(ceilings)
}

fn take_artifact_scan(root: &mut Map<String, Value>) -> Result<ArtifactScan, String> {
    let mut map = take_object(root, "artifactScan")?;
    let scan = ArtifactScan {
        max_depth: take_u64(&mut map, "maxDepth")?,
        max_files: take_u64(&mut map, "maxFiles")?,
        max_file_bytes: take_u64(&mut map, "maxFileBytes")?,
        stderr_head_bytes: take_u64(&mut map, "stderrHeadBytes")?,
        stderr_tail_bytes: take_u64(&mut map, "stderrTailBytes")?,
    };
    deny_unknown(map, "artifactScan")?;
    for (name, value) in [
        ("maxDepth", scan.max_depth),
        ("maxFiles", scan.max_files),
        ("maxFileBytes", scan.max_file_bytes),
        ("stderrHeadBytes", scan.stderr_head_bytes),
        ("stderrTailBytes", scan.stderr_tail_bytes),
    ] {
        if value == 0 {
            return Err(format!("artifactScan.{name} must be greater than zero"));
        }
    }
    if usize::try_from(scan.stderr_head_bytes).is_err()
        || usize::try_from(scan.stderr_tail_bytes).is_err()
    {
        return Err("artifactScan stderr bounds must fit the host usize".into());
    }
    Ok(scan)
}

// ---------------------------------------------------------------------------
// Exact identity snapshot
// ---------------------------------------------------------------------------

/// Redundant count cross-check carried alongside the identity snapshot.
///
/// Counts alone are cardinality-neutral and can never be the whole oracle, but
/// keeping them makes a count regression legible without diffing identities.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnCardinality {
    pub work: usize,
    pub runs: usize,
    pub intents: usize,
}

/// One public attempt with its complete, sorted set of linked Run ids.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptIdentity {
    pub attempt_id: String,
    pub linked_run_ids: Vec<String>,
}

/// One public Work row plus every attempt it exposes.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkIdentity {
    pub work_id: String,
    pub kind: Option<String>,
    pub source_manager_step_id: Option<String>,
    pub state: Option<String>,
    pub attempts: Vec<AttemptIdentity>,
}

/// One execution intent, including the public revision and hash fields that
/// make substitution detectable.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentIdentity {
    pub intent_id: String,
    pub work_id: String,
    pub attempt_id: String,
    pub run_id: String,
    pub input_hash: String,
    pub work_revision: u64,
    pub agent_spec_revision: u64,
}

/// One public Run row.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunIdentity {
    pub run_id: String,
    pub request_id: String,
    pub purpose: Option<String>,
    pub state: Option<String>,
}

/// A deterministic, fully ordered projection of every durable identity the
/// public MCP contract exposes for one session.
///
/// Equality over this structure is the always-on growth oracle. Because every
/// id, every attempt-to-run link, every intent tuple and every run tuple is
/// compared, a cardinality-neutral substitution — a Work rebuilt under a new
/// id, an attempt relinked to a different Run, an intent re-pointed at a
/// replacement Run — fails, where a count comparison passed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnSnapshot {
    pub work: Vec<WorkIdentity>,
    pub intents: Vec<IntentIdentity>,
    pub runs: Vec<RunIdentity>,
    pub counts: AlwaysOnCardinality,
}

impl AlwaysOnSnapshot {
    /// Build a snapshot from the three public list projections plus the
    /// per-Work `ptah_get_work` detail documents, keyed by Work id.
    pub fn build(
        work: &Value,
        details: &BTreeMap<String, Value>,
        intents: &Value,
        runs: &Value,
    ) -> Result<Self, DiagnosticCode> {
        let mut work_rows = Vec::new();
        for item in work_items(work) {
            let work_id = non_empty(&item["workId"])?;
            let detail = details
                .get(&work_id)
                .ok_or(DiagnosticCode::McpResultMalformed)?;
            if detail["work"]["workId"].as_str() != Some(work_id.as_str()) {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            let mut attempts = Vec::new();
            for attempt in exact_array(detail, "attempts")? {
                let attempt_id = non_empty(&attempt["attemptId"])?;
                let mut linked_run_ids = attempt["linkedRunIds"]
                    .as_array()
                    .ok_or(DiagnosticCode::McpResultMalformed)?
                    .iter()
                    .map(non_empty)
                    .collect::<Result<Vec<_>, _>>()?;
                linked_run_ids.sort();
                if linked_run_ids.windows(2).any(|pair| pair[0] == pair[1]) {
                    return Err(DiagnosticCode::StateTransitionMismatch);
                }
                attempts.push(AttemptIdentity {
                    attempt_id,
                    linked_run_ids,
                });
            }
            attempts.sort();
            if attempts
                .windows(2)
                .any(|pair| pair[0].attempt_id == pair[1].attempt_id)
            {
                return Err(DiagnosticCode::StateTransitionMismatch);
            }
            work_rows.push(WorkIdentity {
                work_id,
                kind: optional_string(&item["kind"]),
                source_manager_step_id: optional_string(&item["sourceManagerStepId"]),
                state: optional_string(&item["state"]),
                attempts,
            });
        }
        work_rows.sort();
        if work_rows
            .windows(2)
            .any(|pair| pair[0].work_id == pair[1].work_id)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }

        let mut intent_rows = Vec::new();
        for intent in exact_array(intents, "intents")? {
            intent_rows.push(IntentIdentity {
                intent_id: non_empty(&intent["intentId"])?,
                work_id: non_empty(&intent["workId"])?,
                attempt_id: non_empty(&intent["attemptId"])?,
                run_id: non_empty(&intent["runId"])?,
                input_hash: non_empty(&intent["inputHash"])?,
                work_revision: intent["workRevision"]
                    .as_u64()
                    .ok_or(DiagnosticCode::McpResultMalformed)?,
                agent_spec_revision: intent["agentSpecRevision"]
                    .as_u64()
                    .ok_or(DiagnosticCode::McpResultMalformed)?,
            });
        }
        intent_rows.sort();
        if intent_rows
            .windows(2)
            .any(|pair| pair[0].intent_id == pair[1].intent_id)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }

        let mut run_rows = Vec::new();
        for run in exact_array(runs, "runs")? {
            run_rows.push(RunIdentity {
                run_id: non_empty(&run["runId"])?,
                request_id: non_empty(&run["requestId"])?,
                purpose: optional_string(&run["purpose"]),
                state: optional_string(&run["state"]),
            });
        }
        run_rows.sort();
        if run_rows
            .windows(2)
            .any(|pair| pair[0].run_id == pair[1].run_id)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }

        Ok(Self {
            counts: AlwaysOnCardinality {
                work: work_rows.len(),
                runs: run_rows.len(),
                intents: intent_rows.len(),
            },
            work: work_rows,
            intents: intent_rows,
            runs: run_rows,
        })
    }

    /// Every Work id, sorted.
    pub fn work_ids(&self) -> Vec<&str> {
        self.work.iter().map(|item| item.work_id.as_str()).collect()
    }

    /// The unique Work whose public `sourceManagerStepId` is `step_id`.
    pub fn work_for_step(&self, step_id: &str) -> Result<&WorkIdentity, DiagnosticCode> {
        let matching: Vec<&WorkIdentity> = self
            .work
            .iter()
            .filter(|item| item.source_manager_step_id.as_deref() == Some(step_id))
            .collect();
        match matching.as_slice() {
            [only] => Ok(only),
            _ => Err(DiagnosticCode::StateTransitionMismatch),
        }
    }

    /// The Run with `run_id`, if the projection exposes exactly one.
    pub fn run(&self, run_id: &str) -> Result<&RunIdentity, DiagnosticCode> {
        self.runs
            .iter()
            .find(|run| run.run_id == run_id)
            .ok_or(DiagnosticCode::StateTransitionMismatch)
    }

    /// The Run ids whose public `purpose` is `purpose`, sorted.
    pub fn runs_with_purpose(&self, purpose: &str) -> Vec<&str> {
        self.runs
            .iter()
            .filter(|run| run.purpose.as_deref() == Some(purpose))
            .map(|run| run.run_id.as_str())
            .collect()
    }

    /// The snapshot the interrupted-run fence must observe after a restart:
    /// every identity preserved bit for bit, with exactly the target Work
    /// `failed` and the target Run `interrupted`.
    ///
    /// Expressing the fence this way keeps the Run and Work `state` fields
    /// inside the compared oracle. Dropping state to make the comparison hold
    /// would have re-opened the substitution hole this module closes.
    pub fn with_interruption(&self, work_id: &str, run_id: &str) -> Self {
        let mut next = self.clone();
        for item in &mut next.work {
            if item.work_id == work_id && item.state.is_some() {
                item.state = Some("failed".to_owned());
            }
        }
        for run in &mut next.runs {
            if run.run_id == run_id && run.state.is_some() {
                run.state = Some("interrupted".to_owned());
            }
        }
        next
    }
}

/// Compare two snapshots for exact identity equality.
pub fn assert_exact_snapshot(
    expected: &AlwaysOnSnapshot,
    actual: &AlwaysOnSnapshot,
) -> Result<(), DiagnosticCode> {
    if expected == actual {
        Ok(())
    } else {
        Err(DiagnosticCode::StateTransitionMismatch)
    }
}

/// Compare only the redundant counts.
pub fn assert_exact_cardinality(
    expected: AlwaysOnCardinality,
    actual: AlwaysOnCardinality,
) -> Result<(), DiagnosticCode> {
    if expected == actual {
        Ok(())
    } else {
        Err(DiagnosticCode::StateTransitionMismatch)
    }
}

pub(crate) fn work_items(work: &Value) -> &[Value] {
    work.get("work")
        .or_else(|| work.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub(crate) fn exact_array<'a>(value: &'a Value, key: &str) -> Result<&'a [Value], DiagnosticCode> {
    value[key]
        .as_array()
        .map(Vec::as_slice)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

fn non_empty(value: &Value) -> Result<String, DiagnosticCode> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(DiagnosticCode::McpResultMalformed)
}

fn optional_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Bootstrap baseline
// ---------------------------------------------------------------------------

/// Assert that a freshly bootstrapped home contains exactly the bootstrap lane
/// and nothing else.
///
/// The always-on probe submits one setup task before creating the manager
/// plan, so the session is not empty when the plan starts. Treating whatever
/// happens to be present as "the baseline" would let a polluted home — a
/// leftover Work, a resurrected Run, a stray intent — be absorbed into the
/// accepted starting state and never fail. Instead the baseline must be closed
/// over the single setup Run whose id the probe already holds: exactly one Run
/// and that Run is the setup Run, at most one Work and it links only to the
/// setup Run, at most one intent and it points at both.
pub fn assert_bootstrap_baseline(
    baseline: &AlwaysOnSnapshot,
    fixture: &AlwaysOnFixture,
    setup_run_id: &str,
) -> Result<(), DiagnosticCode> {
    let [run] = baseline.runs.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    if run.run_id != setup_run_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if baseline.work.len() > 1 || baseline.intents.len() > 1 {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let reserved: BTreeSet<String> = fixture
        .native_steps()
        .into_iter()
        .chain([MANAGER_DECISION_STEP_ID.to_owned()])
        .collect();
    for item in &baseline.work {
        if item
            .source_manager_step_id
            .as_deref()
            .is_some_and(|step| reserved.contains(step))
            || item.kind.as_deref() == Some(MANAGER_DECISION_KIND)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if item.attempts.is_empty() {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        if item
            .attempts
            .iter()
            .any(|attempt| attempt.linked_run_ids != vec![setup_run_id.to_owned()])
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    for intent in &baseline.intents {
        let Some(item) = baseline.work.first() else {
            return Err(DiagnosticCode::StateTransitionMismatch);
        };
        if intent.run_id != setup_run_id
            || intent.work_id != item.work_id
            || !item
                .attempts
                .iter()
                .any(|attempt| attempt.attempt_id == intent.attempt_id)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan lanes and the manager-decision binding
// ---------------------------------------------------------------------------

/// The public identity chain for one plan lane:
/// Work -> single attempt -> single linked Run -> intent -> Run.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnLane {
    pub step_id: String,
    pub work_id: String,
    pub attempt_id: String,
    pub intent_id: String,
    pub run_id: String,
}

/// One lane's identity chain, opaque for publication.
///
/// The report is scanned for forbidden data before it is written, and raw
/// durable identifiers are token-shaped, so every id published here is the
/// `opaque-<sha256>` digest the rest of the report already uses. The digests
/// are still exact: two lanes that differ anywhere produce different evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnLaneEvidence {
    pub step_id: String,
    pub work: String,
    pub attempt: String,
    pub intent: String,
    pub run: String,
}

impl AlwaysOnLane {
    /// Project this lane into the opaque form the report publishes.
    pub fn evidence(&self) -> AlwaysOnLaneEvidence {
        AlwaysOnLaneEvidence {
            step_id: self.step_id.clone(),
            work: opaque_durable_id(&self.work_id),
            attempt: opaque_durable_id(&self.attempt_id),
            intent: opaque_durable_id(&self.intent_id),
            run: opaque_durable_id(&self.run_id),
        }
    }
}

/// Whether the manager-decision Work could be causally bound to the manager
/// proposal Run using only public identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagerDecisionBinding {
    /// The decision Work's own Run is the unique Run whose public `purpose` is
    /// `manager_proposal`. The causal oracle is proven end to end.
    Bound { lane: AlwaysOnLaneEvidence },
    /// No Run in the public projection carries a `purpose`, so decision Work
    /// and proposal Run cannot be joined through the public contract.
    ///
    /// This arm is a documented boundary, never a pass: the fixture declares
    /// `proposalRunsObserved`, which is unprovable without the projection, so
    /// the probe fails closed rather than claim an unearned causal oracle.
    PurposeNotProjected { work: String },
}

impl ManagerDecisionBinding {
    /// True only when public identities actually proved the join.
    pub fn is_bound(&self) -> bool {
        matches!(self, Self::Bound { .. })
    }
}

/// Everything the happy-path shape check proved, carried into the report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlwaysOnHappyShape {
    pub native_lanes: Vec<AlwaysOnLaneEvidence>,
    pub decision_lane: AlwaysOnLaneEvidence,
    pub manager_decision_binding: ManagerDecisionBinding,
}

/// The terminal public Work state the fixture's DAG requires for `step_id`.
///
/// The fixture's middle step is a forced failure the manager must replace, so
/// a blanket "every native step succeeded" expectation can never hold on a
/// healthy run. Deriving the terminal state per step from the fixture keeps
/// the oracle exact instead of relaxing it to "any terminal state".
pub fn expected_step_state(
    fixture: &AlwaysOnFixture,
    step_id: &str,
) -> Result<&'static str, DiagnosticCode> {
    if step_id == fixture.step_first || step_id == fixture.step_replacement {
        Ok("succeeded")
    } else if step_id == fixture.step_failing {
        Ok("failed")
    } else if step_id == MANAGER_DECISION_STEP_ID {
        Ok("succeeded")
    } else {
        Err(DiagnosticCode::FixtureInvalid)
    }
}

/// The single Work row the manager plan itself occupies.
///
/// `ptah_list_work` projects the plan alongside its step lanes. It carries no
/// `sourceManagerStepId` and no attempts, so it belongs to neither the
/// bootstrap baseline nor any lane and must be accounted for explicitly.
pub fn require_plan_work(snapshot: &AlwaysOnSnapshot) -> Result<&WorkIdentity, DiagnosticCode> {
    let matching: Vec<&WorkIdentity> = snapshot
        .work
        .iter()
        .filter(|item| item.kind.as_deref() == Some(MANAGER_PLAN_KIND))
        .collect();
    match matching.as_slice() {
        [only] if only.source_manager_step_id.is_none() && only.attempts.is_empty() => Ok(only),
        _ => Err(DiagnosticCode::StateTransitionMismatch),
    }
}

/// Resolve the full public identity chain for the Work projected under
/// `step_id`, requiring exactly one Work, one attempt, one linked Run, one
/// intent and one Run.
pub fn resolve_lane(
    snapshot: &AlwaysOnSnapshot,
    step_id: &str,
) -> Result<AlwaysOnLane, DiagnosticCode> {
    let item = snapshot.work_for_step(step_id)?;
    let [attempt] = item.attempts.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    let [run_id] = attempt.linked_run_ids.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    let matching: Vec<&IntentIdentity> = snapshot
        .intents
        .iter()
        .filter(|intent| intent.work_id == item.work_id && intent.attempt_id == attempt.attempt_id)
        .collect();
    let [intent] = matching.as_slice() else {
        return Err(DiagnosticCode::StateTransitionMismatch);
    };
    if intent.run_id != *run_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let run = snapshot.run(run_id)?;
    if run.request_id != intent.intent_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(AlwaysOnLane {
        step_id: step_id.to_owned(),
        work_id: item.work_id.clone(),
        attempt_id: attempt.attempt_id.clone(),
        intent_id: intent.intent_id.clone(),
        run_id: run_id.clone(),
    })
}

/// Bind the manager-decision Work to the manager proposal Run using only
/// public identities.
///
/// The join is real: `ptah_list_work` projects the decision lane under the
/// reserved step id `__manager_decision__`, that Work carries exactly one
/// attempt with exactly one linked Run, and `ptah_list_runs` projects
/// `purpose` on that Run. Binding therefore replaces two independent counts
/// ("one decision Work exists" and "one proposal Run exists") with a proof
/// that they are the same causal chain.
pub fn bind_manager_decision(
    snapshot: &AlwaysOnSnapshot,
) -> Result<(AlwaysOnLane, ManagerDecisionBinding), DiagnosticCode> {
    let lane = resolve_lane(snapshot, MANAGER_DECISION_STEP_ID)?;
    let work = snapshot.work_for_step(MANAGER_DECISION_STEP_ID)?;
    if work.kind.as_deref() != Some(MANAGER_DECISION_KIND) {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if snapshot.runs.iter().all(|run| run.purpose.is_none()) {
        let binding = ManagerDecisionBinding::PurposeNotProjected {
            work: opaque_durable_id(&lane.work_id),
        };
        return Ok((lane, binding));
    }
    let proposals = snapshot.runs_with_purpose(MANAGER_PROPOSAL_PURPOSE);
    if proposals != vec![lane.run_id.as_str()] {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let binding = ManagerDecisionBinding::Bound {
        lane: lane.evidence(),
    };
    Ok((lane, binding))
}

/// Terminal public Work states.
const TERMINAL_WORK_STATES: &[&str] = &["succeeded", "failed", "cancelled", "interrupted"];
/// Terminal public Run states.
pub const TERMINAL_RUN_STATES: &[&str] = &[
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "limit_reached",
];

/// True when every identity in the bootstrap baseline has settled, so its
/// states can be compared byte for byte later without racing the supervisor.
pub fn baseline_is_settled(baseline: &AlwaysOnSnapshot) -> bool {
    baseline.work.iter().all(|item| {
        item.state
            .as_deref()
            .is_some_and(|state| TERMINAL_WORK_STATES.contains(&state))
    }) && baseline.runs.iter().all(|run| {
        run.state
            .as_deref()
            .is_some_and(|state| TERMINAL_RUN_STATES.contains(&state))
    })
}

/// Drop every identity belonging to `lanes`, leaving what the plan did not
/// create.
fn residual(
    snapshot: &AlwaysOnSnapshot,
    lanes: &[&AlwaysOnLane],
    extra_work: &[&str],
) -> AlwaysOnSnapshot {
    let work_ids: BTreeSet<&str> = lanes
        .iter()
        .map(|lane| lane.work_id.as_str())
        .chain(extra_work.iter().copied())
        .collect();
    let intent_ids: BTreeSet<&str> = lanes.iter().map(|lane| lane.intent_id.as_str()).collect();
    let run_ids: BTreeSet<&str> = lanes.iter().map(|lane| lane.run_id.as_str()).collect();
    let work: Vec<WorkIdentity> = snapshot
        .work
        .iter()
        .filter(|item| !work_ids.contains(item.work_id.as_str()))
        .cloned()
        .collect();
    let intents: Vec<IntentIdentity> = snapshot
        .intents
        .iter()
        .filter(|intent| !intent_ids.contains(intent.intent_id.as_str()))
        .cloned()
        .collect();
    let runs: Vec<RunIdentity> = snapshot
        .runs
        .iter()
        .filter(|run| !run_ids.contains(run.run_id.as_str()))
        .cloned()
        .collect();
    AlwaysOnSnapshot {
        counts: AlwaysOnCardinality {
            work: work.len(),
            runs: runs.len(),
            intents: intents.len(),
        },
        work,
        intents,
        runs,
    }
}

/// Total native Work the fixture declares across its three steps.
fn declared_native_work(fixture: &AlwaysOnFixture) -> Result<u64, DiagnosticCode> {
    fixture
        .native_work_by_step
        .values()
        .try_fold(0_u64, |total, count| total.checked_add(*count))
        .ok_or(DiagnosticCode::FixtureInvalid)
}

fn widen(value: u64) -> Result<usize, DiagnosticCode> {
    usize::try_from(value).map_err(|_| DiagnosticCode::FixtureInvalid)
}

fn sum(base: usize, added: usize) -> Result<usize, DiagnosticCode> {
    base.checked_add(added).ok_or(DiagnosticCode::BoundExceeded)
}

/// The counts the completed happy path must reach: the recorded bootstrap
/// baseline plus one lane per declared native step and the single
/// manager-decision lane, whose Run is the manager proposal Run.
///
/// This is a redundant cross-check on top of the identity comparison, not the
/// oracle: counts alone cannot detect substitution.
pub fn expected_happy_cardinality(
    fixture: &AlwaysOnFixture,
    baseline: AlwaysOnCardinality,
) -> Result<AlwaysOnCardinality, DiagnosticCode> {
    let native = widen(declared_native_work(fixture)?)?;
    let decision = widen(fixture.decision_work)?;
    let proposal = widen(fixture.proposal_runs)?;
    Ok(AlwaysOnCardinality {
        work: sum(
            baseline.work,
            sum(sum(native, decision)?, MANAGER_PLAN_WORK_ROWS)?,
        )?,
        runs: sum(baseline.runs, sum(native, proposal)?)?,
        intents: sum(baseline.intents, sum(native, decision)?)?,
    })
}

/// The counts the held Home-B session must show before the first restart: the
/// bootstrap baseline plus exactly the one held lane.
pub fn expected_pre_restart_cardinality(
    fixture: &AlwaysOnFixture,
    baseline: AlwaysOnCardinality,
) -> Result<AlwaysOnCardinality, DiagnosticCode> {
    let held = widen(
        fixture
            .native_work_by_step
            .get(&fixture.step_first)
            .copied()
            .ok_or(DiagnosticCode::FixtureInvalid)?,
    )?;
    Ok(AlwaysOnCardinality {
        work: sum(baseline.work, sum(held, MANAGER_PLAN_WORK_ROWS)?)?,
        runs: sum(baseline.runs, held)?,
        intents: sum(baseline.intents, held)?,
    })
}

/// Assert the completed happy path has exactly the fixture-derived shape:
/// one lane per declared native step, the declared manager-decision lane bound
/// to the manager proposal Run, and nothing beyond the recorded bootstrap
/// baseline.
pub fn assert_happy_shape(
    fixture: &AlwaysOnFixture,
    baseline: &AlwaysOnSnapshot,
    final_snapshot: &AlwaysOnSnapshot,
) -> Result<AlwaysOnHappyShape, DiagnosticCode> {
    let mut native_lanes = Vec::new();
    for step in fixture.native_steps() {
        if fixture.native_work_by_step.get(&step) != Some(&1) {
            return Err(DiagnosticCode::FixtureInvalid);
        }
        let lane = resolve_lane(final_snapshot, &step)?;
        if final_snapshot.work_for_step(&step)?.state.as_deref()
            != Some(expected_step_state(fixture, &step)?)
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
        native_lanes.push(lane);
    }
    if fixture.decision_work != 1 || fixture.proposal_runs != 1 {
        return Err(DiagnosticCode::FixtureInvalid);
    }
    let (decision_lane, manager_decision_binding) = bind_manager_decision(final_snapshot)?;
    if final_snapshot
        .work_for_step(MANAGER_DECISION_STEP_ID)?
        .state
        .as_deref()
        != Some(expected_step_state(fixture, MANAGER_DECISION_STEP_ID)?)
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if !manager_decision_binding.is_bound() {
        // The fixture asserts one observed proposal Run; without the public
        // `purpose` projection that claim cannot be proven, so fail closed
        // instead of recording an oracle the evidence does not support.
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let lanes: Vec<&AlwaysOnLane> = native_lanes.iter().chain([&decision_lane]).collect();
    let distinct: BTreeSet<&str> = lanes.iter().map(|lane| lane.work_id.as_str()).collect();
    if distinct.len() != lanes.len() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let plan_work = require_plan_work(final_snapshot)?.work_id.clone();
    assert_exact_snapshot(
        baseline,
        &residual(final_snapshot, &lanes, &[plan_work.as_str()]),
    )?;
    assert_exact_cardinality(
        expected_happy_cardinality(fixture, baseline.counts)?,
        final_snapshot.counts,
    )?;
    Ok(AlwaysOnHappyShape {
        native_lanes: native_lanes.iter().map(AlwaysOnLane::evidence).collect(),
        decision_lane: decision_lane.evidence(),
        manager_decision_binding,
    })
}

/// Assert the fresh Home-B session, captured while the first native step is
/// held open at the provider, has exactly the fixture-derived pre-restart
/// shape.
///
/// The previous oracle accepted any non-zero counts here, so a home carrying
/// leftover Work, Runs or intents silently became the accepted baseline and
/// every later zero-growth comparison inherited the pollution. This requires
/// exactly the held lane plus the recorded bootstrap baseline: the dependent
/// step, the replacement step and the manager-decision lane must all still be
/// absent.
pub fn assert_home_b_pre_restart_shape(
    fixture: &AlwaysOnFixture,
    baseline: &AlwaysOnSnapshot,
    pre_restart: &AlwaysOnSnapshot,
    work_id: &str,
    attempt_id: &str,
    run_id: &str,
) -> Result<(), DiagnosticCode> {
    let lane = resolve_lane(pre_restart, &fixture.step_first)?;
    if lane.work_id != work_id || lane.attempt_id != attempt_id || lane.run_id != run_id {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    if fixture.native_work_by_step.get(&fixture.step_first) != Some(&1) {
        return Err(DiagnosticCode::FixtureInvalid);
    }
    for absent in [
        fixture.step_failing.as_str(),
        fixture.step_replacement.as_str(),
        MANAGER_DECISION_STEP_ID,
    ] {
        if pre_restart
            .work
            .iter()
            .any(|item| item.source_manager_step_id.as_deref() == Some(absent))
        {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    if pre_restart
        .work
        .iter()
        .any(|item| item.kind.as_deref() == Some(MANAGER_DECISION_KIND))
        || !pre_restart
            .runs_with_purpose(MANAGER_PROPOSAL_PURPOSE)
            .is_empty()
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let plan_work = require_plan_work(pre_restart)?.work_id.clone();
    assert_exact_snapshot(
        baseline,
        &residual(pre_restart, &[&lane], &[plan_work.as_str()]),
    )?;
    assert_exact_cardinality(
        expected_pre_restart_cardinality(fixture, baseline.counts)?,
        pre_restart.counts,
    )
}

/// Assert the held home reached exactly the steady state a restart must
/// produce.
///
/// Comparing the post-restart snapshot against the pre-restart one directly
/// can never hold: when the held step fails, the autonomous manager reacts
/// once, materialising a manager-decision lane bound to a manager proposal
/// Run. The real invariant is that *nothing else* moves — every pre-restart
/// identity survives, the held lane lands on failed Work over an interrupted
/// Run, and the manager's reaction is exactly one fully joined decision lane.
pub fn assert_post_restart_shape(
    pre_restart: &AlwaysOnSnapshot,
    post_restart: &AlwaysOnSnapshot,
    work_id: &str,
    run_id: &str,
) -> Result<AlwaysOnLane, DiagnosticCode> {
    let (decision_lane, binding) = bind_manager_decision(post_restart)?;
    if !binding.is_bound() {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let decision_work = post_restart.work_for_step(MANAGER_DECISION_STEP_ID)?;
    if decision_work.state.as_deref() != Some("succeeded") {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    assert_exact_snapshot(
        &pre_restart.with_interruption(work_id, run_id),
        &residual(post_restart, &[&decision_lane], &[]),
    )?;
    Ok(decision_lane)
}

// ---------------------------------------------------------------------------
// Loopback provider lanes
// ---------------------------------------------------------------------------

/// Which isolated `GROKPTAH_HOME` produced a set of loopback observations.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlwaysOnHome {
    /// The uninterrupted happy-path home.
    HomeA,
    /// The held-then-restarted home.
    HomeB,
}

impl AlwaysOnHome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HomeA => "home_a",
            Self::HomeB => "home_b",
        }
    }
}

/// One home's complete loopback provider observation, kept separable so the
/// evidence for each home stays reconstructable from the public report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopbackProviderLane {
    pub home: AlwaysOnHome,
    pub accepted_posts: u64,
    pub rejected_auth: u64,
    pub records: Vec<LoopbackProviderRecord>,
}

impl LoopbackProviderLane {
    pub fn new(home: AlwaysOnHome, observation: LoopbackProviderObservation) -> Self {
        Self {
            home,
            accepted_posts: observation.accepted_posts,
            rejected_auth: observation.rejected_auth,
            records: observation.records,
        }
    }

    /// Accepted, correctly routed POSTs carrying `semantic_id`.
    pub fn accepted_for(&self, semantic_id: &str) -> u64 {
        self.records
            .iter()
            .filter(|record| {
                record.auth_accepted && record.route_ok && record.semantic_id == semantic_id
            })
            .count() as u64
    }
}

/// Merge every home's lane into the single public observation field.
///
/// Home B used to overwrite the field outright, so a passing report carried
/// only the restarted home and every Home-A oracle — plan success, the three
/// native causal joins, replay and conflict idempotency — rested on provider
/// records the report no longer contained. Merging keeps both, ordered by
/// home, and the per-lane breakdown keeps them attributable.
pub fn merge_provider_lanes(lanes: &[LoopbackProviderLane]) -> Option<LoopbackProviderObservation> {
    if lanes.is_empty() {
        return None;
    }
    let mut ordered: Vec<&LoopbackProviderLane> = lanes.iter().collect();
    ordered.sort_by_key(|lane| lane.home);
    Some(LoopbackProviderObservation {
        accepted_posts: ordered.iter().fold(0_u64, |total, lane| {
            total.saturating_add(lane.accepted_posts)
        }),
        rejected_auth: ordered.iter().fold(0_u64, |total, lane| {
            total.saturating_add(lane.rejected_auth)
        }),
        records: ordered
            .iter()
            .flat_map(|lane| lane.records.iter().cloned())
            .collect(),
    })
}

/// Assert both homes reported the exact provider semantics the fixture
/// declares.
///
/// Home A must show one accepted POST for every declared semantic id — the
/// three native steps and the manager decision — plus the single bootstrap
/// `setup` POST. Home B holds the first step open and is restarted twice, so
/// it must show its own `setup` POST, exactly one POST for the held step, and
/// exactly one manager-decision POST for the single reaction to that step
/// failing. It must never have reached the dependent step or its replacement,
/// and the held step must never be re-sent across either restart.
pub fn assert_provider_lanes(
    fixture: &AlwaysOnFixture,
    lanes: &[LoopbackProviderLane],
) -> Result<(), DiagnosticCode> {
    let home_a = require_lane(lanes, AlwaysOnHome::HomeA)?;
    let home_b = require_lane(lanes, AlwaysOnHome::HomeB)?;
    for (semantic, expected) in &fixture.provider_posts_by_semantic {
        if home_a.accepted_for(semantic) != *expected {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    if home_a.accepted_for(SETUP_SEMANTIC_ID) != 1
        || home_a.accepted_posts != fixture.expected_total_posts()?
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    let held = fixture
        .posts_for(&fixture.step_first)
        .ok_or(DiagnosticCode::FixtureInvalid)?;
    let decision = fixture
        .posts_for(MANAGER_DECISION_KIND)
        .ok_or(DiagnosticCode::FixtureInvalid)?;
    if home_b.accepted_for(SETUP_SEMANTIC_ID) != 1
        || home_b.accepted_for(&fixture.step_first) != held
        || home_b.accepted_for(MANAGER_DECISION_KIND) != decision
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    // Home B never reaches the dependent step or its replacement: the held
    // lane fails, the manager reacts exactly once, and the plan stops there.
    for never in [
        fixture.step_failing.as_str(),
        fixture.step_replacement.as_str(),
    ] {
        if home_b.accepted_for(never) != 0 {
            return Err(DiagnosticCode::StateTransitionMismatch);
        }
    }
    if home_b.accepted_posts
        != held
            .checked_add(decision)
            .and_then(|total| total.checked_add(1))
            .ok_or(DiagnosticCode::FixtureInvalid)?
    {
        return Err(DiagnosticCode::StateTransitionMismatch);
    }
    Ok(())
}

fn require_lane(
    lanes: &[LoopbackProviderLane],
    home: AlwaysOnHome,
) -> Result<&LoopbackProviderLane, DiagnosticCode> {
    let matching: Vec<&LoopbackProviderLane> =
        lanes.iter().filter(|lane| lane.home == home).collect();
    match matching.as_slice() {
        [only] if !only.records.is_empty() => Ok(only),
        _ => Err(DiagnosticCode::ProviderObservationDropped),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SETUP_WORK: &str = "work-setup";
    const SETUP_ATTEMPT: &str = "attempt-setup";
    const SETUP_RUN: &str = "run-setup";
    const PLAN_WORK: &str = "work-plan";

    fn fixture() -> AlwaysOnFixture {
        AlwaysOnFixture::parse(crate::ALWAYS_ON_GROKBOT_FIXTURE).expect("canonical fixture")
    }

    fn canonical_value() -> Value {
        parse_duplicate_free(crate::ALWAYS_ON_GROKBOT_FIXTURE).expect("canonical fixture")
    }

    /// One lane's public projection rows.
    struct Lane {
        step: &'static str,
        kind: &'static str,
        work: &'static str,
        attempt: &'static str,
        intent: &'static str,
        run: &'static str,
        purpose: Option<&'static str>,
        state: &'static str,
    }

    fn happy_lanes() -> Vec<Lane> {
        vec![
            Lane {
                step: "step-a",
                kind: "native",
                work: "work-a",
                attempt: "attempt-a",
                intent: "intent-a",
                run: "run-a",
                purpose: Some("native"),
                state: "succeeded",
            },
            Lane {
                step: "step-b",
                kind: "native",
                work: "work-b",
                attempt: "attempt-b",
                intent: "intent-b",
                run: "run-b",
                purpose: Some("native"),
                // The fixture's middle step is the forced failure.
                state: "failed",
            },
            Lane {
                step: "step-b-fix",
                kind: "native",
                work: "work-c",
                attempt: "attempt-c",
                intent: "intent-c",
                run: "run-c",
                purpose: Some("native"),
                state: "succeeded",
            },
            Lane {
                step: MANAGER_DECISION_STEP_ID,
                kind: MANAGER_DECISION_KIND,
                work: "work-d",
                attempt: "attempt-d",
                intent: "intent-d",
                run: "run-d",
                purpose: Some(MANAGER_PROPOSAL_PURPOSE),
                state: "succeeded",
            },
        ]
    }

    /// Build the three list projections plus the per-Work detail documents for
    /// the bootstrap baseline followed by `lanes`.
    fn projection(
        lanes: &[Lane],
        with_baseline: bool,
    ) -> (Value, BTreeMap<String, Value>, Value, Value) {
        projection_with(lanes, with_baseline, true)
    }

    /// Build the three list projections plus the per-Work detail documents.
    ///
    /// `with_plan` adds the Work row the manager plan itself occupies, which
    /// the real `ptah_list_work` projection carries alongside the step lanes.
    fn projection_with(
        lanes: &[Lane],
        with_baseline: bool,
        with_plan: bool,
    ) -> (Value, BTreeMap<String, Value>, Value, Value) {
        let mut work = Vec::new();
        let mut details = BTreeMap::new();
        let mut intents = Vec::new();
        let mut runs = Vec::new();
        if with_baseline {
            work.push(json!({
                "workId": SETUP_WORK,
                "kind": "native",
                "state": "succeeded"
            }));
            details.insert(
                SETUP_WORK.to_owned(),
                json!({
                    "work": {"workId": SETUP_WORK},
                    "attempts": [{
                        "attemptId": SETUP_ATTEMPT,
                        "linkedRunIds": [SETUP_RUN]
                    }]
                }),
            );
            runs.push(json!({
                "runId": SETUP_RUN,
                "requestId": "req-setup",
                "purpose": "native",
                "state": "completed"
            }));
        }
        if with_plan {
            work.push(json!({
                "workId": PLAN_WORK,
                "kind": MANAGER_PLAN_KIND,
                "state": "blocked"
            }));
            details.insert(
                PLAN_WORK.to_owned(),
                json!({"work": {"workId": PLAN_WORK}, "attempts": []}),
            );
        }
        for lane in lanes {
            work.push(json!({
                "workId": lane.work,
                "kind": lane.kind,
                "sourceManagerStepId": lane.step,
                "state": lane.state
            }));
            details.insert(
                lane.work.to_owned(),
                json!({
                    "work": {"workId": lane.work},
                    "attempts": [{
                        "attemptId": lane.attempt,
                        "linkedRunIds": [lane.run]
                    }]
                }),
            );
            intents.push(json!({
                "intentId": lane.intent,
                "workId": lane.work,
                "attemptId": lane.attempt,
                "runId": lane.run,
                "inputHash": format!("hash-{}", lane.work),
                "workRevision": 1,
                "agentSpecRevision": 1
            }));
            runs.push(json!({
                "runId": lane.run,
                "requestId": lane.intent,
                "purpose": lane.purpose,
                "state": "completed"
            }));
        }
        (
            json!({"work": work}),
            details,
            json!({"intents": intents}),
            json!({"runs": runs}),
        )
    }

    fn snapshot(lanes: &[Lane], with_baseline: bool) -> AlwaysOnSnapshot {
        let (work, details, intents, runs) = projection(lanes, with_baseline);
        AlwaysOnSnapshot::build(&work, &details, &intents, &runs).expect("snapshot")
    }

    fn baseline() -> AlwaysOnSnapshot {
        let (work, details, intents, runs) = projection_with(&[], true, false);
        AlwaysOnSnapshot::build(&work, &details, &intents, &runs).expect("baseline snapshot")
    }

    fn happy() -> AlwaysOnSnapshot {
        snapshot(&happy_lanes(), true)
    }

    // -- cardinality-neutral substitution ------------------------------------

    /// Rebuild the canonical happy projection after `mutate` has rewritten it.
    fn mutated(
        mutate: impl FnOnce(&mut Value, &mut BTreeMap<String, Value>, &mut Value, &mut Value),
    ) -> AlwaysOnSnapshot {
        let (mut work, mut details, mut intents, mut runs) = projection(&happy_lanes(), true);
        mutate(&mut work, &mut details, &mut intents, &mut runs);
        AlwaysOnSnapshot::build(&work, &details, &intents, &runs).expect("mutated snapshot")
    }

    #[test]
    fn identity_snapshot_rejects_every_cardinality_neutral_substitution() {
        let canonical = happy();
        let mutants: Vec<(&str, AlwaysOnSnapshot)> = vec![
            (
                "work id replaced",
                mutated(|work, details, intents, _| {
                    work["work"][2]["workId"] = json!("work-a-prime");
                    let detail = details.remove("work-a").unwrap();
                    details.insert(
                        "work-a-prime".into(),
                        json!({
                            "work": {"workId": "work-a-prime"},
                            "attempts": detail["attempts"].clone()
                        }),
                    );
                    intents["intents"][0]["workId"] = json!("work-a-prime");
                }),
            ),
            (
                "attempt id replaced",
                mutated(|_, details, intents, _| {
                    details.get_mut("work-a").unwrap()["attempts"][0]["attemptId"] =
                        json!("attempt-a-prime");
                    intents["intents"][0]["attemptId"] = json!("attempt-a-prime");
                }),
            ),
            (
                "linked run replaced",
                mutated(|_, details, _, _| {
                    details.get_mut("work-a").unwrap()["attempts"][0]["linkedRunIds"] =
                        json!(["run-a-prime"]);
                }),
            ),
            (
                "intent id replaced",
                mutated(|_, _, intents, runs| {
                    intents["intents"][0]["intentId"] = json!("intent-a-prime");
                    runs["runs"][1]["requestId"] = json!("intent-a-prime");
                }),
            ),
            (
                "intent run repointed",
                mutated(|_, _, intents, _| {
                    intents["intents"][0]["runId"] = json!("run-c");
                }),
            ),
            (
                "intent input hash rewritten",
                mutated(|_, _, intents, _| {
                    intents["intents"][0]["inputHash"] = json!("hash-substituted");
                }),
            ),
            (
                "intent work revision rewritten",
                mutated(|_, _, intents, _| {
                    intents["intents"][0]["workRevision"] = json!(2);
                }),
            ),
            (
                "intent agent spec revision rewritten",
                mutated(|_, _, intents, _| {
                    intents["intents"][0]["agentSpecRevision"] = json!(7);
                }),
            ),
            (
                "run id replaced",
                mutated(|_, _, _, runs| {
                    runs["runs"][1]["runId"] = json!("run-a-prime");
                }),
            ),
            (
                "run request id replaced",
                mutated(|_, _, _, runs| {
                    runs["runs"][1]["requestId"] = json!("intent-substituted");
                }),
            ),
            (
                "run state rewritten",
                mutated(|_, _, _, runs| {
                    runs["runs"][1]["state"] = json!("failed");
                }),
            ),
            (
                "run purpose rewritten",
                mutated(|_, _, _, runs| {
                    runs["runs"][4]["purpose"] = json!("native");
                }),
            ),
            (
                "work kind rewritten",
                mutated(|work, _, _, _| {
                    work["work"][5]["kind"] = json!("native");
                }),
            ),
            (
                "work step reassigned",
                mutated(|work, _, _, _| {
                    work["work"][3]["sourceManagerStepId"] = json!("step-substituted");
                }),
            ),
        ];
        for (label, mutant) in mutants {
            assert_eq!(
                mutant.counts, canonical.counts,
                "{label} must stay cardinality-neutral to be a real test"
            );
            assert_eq!(
                assert_exact_cardinality(canonical.counts, mutant.counts),
                Ok(()),
                "{label} must be invisible to the count oracle"
            );
            assert_eq!(
                assert_exact_snapshot(&canonical, &mutant),
                Err(DiagnosticCode::StateTransitionMismatch),
                "{label} must fail the identity oracle"
            );
        }
    }

    #[test]
    fn identity_snapshot_rejects_duplicate_and_malformed_projections() {
        let (work, mut details, intents, runs) = projection(&happy_lanes(), true);
        details.get_mut("work-a").unwrap()["attempts"][0]["linkedRunIds"] =
            json!(["run-a", "run-a"]);
        assert_eq!(
            AlwaysOnSnapshot::build(&work, &details, &intents, &runs),
            Err(DiagnosticCode::StateTransitionMismatch),
            "a repeated linked run must fail"
        );
        let (mut work, details, intents, runs) = projection(&happy_lanes(), true);
        let duplicate = work["work"][2].clone();
        work["work"].as_array_mut().unwrap().push(duplicate);
        assert_eq!(
            AlwaysOnSnapshot::build(&work, &details, &intents, &runs),
            Err(DiagnosticCode::StateTransitionMismatch),
            "a repeated Work row must fail"
        );
        let (work, mut details, intents, runs) = projection(&happy_lanes(), true);
        details.get_mut("work-a").unwrap()["work"]["workId"] = json!("work-other");
        assert_eq!(
            AlwaysOnSnapshot::build(&work, &details, &intents, &runs),
            Err(DiagnosticCode::StateTransitionMismatch),
            "a get_work answering for another Work must fail"
        );
        let (work, details, mut intents, runs) = projection(&happy_lanes(), true);
        intents["intents"][0]["inputHash"] = json!("");
        assert_eq!(
            AlwaysOnSnapshot::build(&work, &details, &intents, &runs),
            Err(DiagnosticCode::McpResultMalformed),
            "an empty inputHash must fail"
        );
    }

    #[test]
    fn interruption_expectation_moves_only_the_held_lane() {
        let canonical = happy();
        let expected = canonical.with_interruption("work-a", "run-a");
        assert_ne!(expected, canonical);
        assert_eq!(
            expected.work_for_step("step-a").unwrap().state.as_deref(),
            Some("failed")
        );
        assert_eq!(
            expected.run("run-a").unwrap().state.as_deref(),
            Some("interrupted")
        );
        // Everything else is untouched, so a second lane drifting still fails.
        assert_eq!(
            expected
                .work_for_step("step-b-fix")
                .unwrap()
                .state
                .as_deref(),
            Some("succeeded")
        );
        assert_eq!(
            expected.run("run-c").unwrap().state.as_deref(),
            Some("completed")
        );
        assert_eq!(expected.counts, canonical.counts);
        let drifted = canonical
            .with_interruption("work-a", "run-a")
            .with_interruption("work-c", "run-c");
        assert_eq!(
            assert_exact_snapshot(&expected, &drifted),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    // -- bootstrap baseline --------------------------------------------------

    #[test]
    fn bootstrap_baseline_accepts_only_the_setup_lane() {
        let fixture = fixture();
        assert_eq!(
            assert_bootstrap_baseline(&baseline(), &fixture, SETUP_RUN),
            Ok(())
        );
        // A home with no Work or intents at all is still a valid bootstrap
        // lane, so the oracle does not depend on whether a direct submit
        // materialises Work.
        let bare = AlwaysOnSnapshot::build(
            &json!({"work": []}),
            &BTreeMap::new(),
            &json!({"intents": []}),
            &json!({"runs": [{
                "runId": SETUP_RUN, "requestId": "req-setup", "state": "completed"
            }]}),
        )
        .unwrap();
        assert_eq!(
            assert_bootstrap_baseline(&bare, &fixture, SETUP_RUN),
            Ok(())
        );
    }

    #[test]
    fn bootstrap_baseline_rejects_every_polluted_home() {
        let fixture = fixture();
        let (work, details, intents, runs) = projection_with(&[], true, false);

        let mut extra_runs = runs.clone();
        extra_runs["runs"].as_array_mut().unwrap().push(json!({
            "runId": "run-stale", "requestId": "req-stale", "state": "completed"
        }));
        let polluted_run = AlwaysOnSnapshot::build(&work, &details, &intents, &extra_runs).unwrap();

        let mut extra_work = work.clone();
        extra_work["work"].as_array_mut().unwrap().push(json!({
            "workId": "work-stale", "kind": "native", "state": "succeeded"
        }));
        let mut extra_details = details.clone();
        extra_details.insert(
            "work-stale".into(),
            json!({
                "work": {"workId": "work-stale"},
                "attempts": [{"attemptId": "attempt-stale", "linkedRunIds": ["run-stale"]}]
            }),
        );
        let mut two_runs = runs.clone();
        two_runs["runs"].as_array_mut().unwrap().push(json!({
            "runId": "run-stale", "requestId": "req-stale", "state": "completed"
        }));
        let polluted_work =
            AlwaysOnSnapshot::build(&extra_work, &extra_details, &intents, &two_runs).unwrap();

        let mut foreign_details = details.clone();
        foreign_details.get_mut(SETUP_WORK).unwrap()["attempts"][0]["linkedRunIds"] =
            json!(["run-elsewhere"]);
        let foreign_link =
            AlwaysOnSnapshot::build(&work, &foreign_details, &intents, &runs).unwrap();

        let mut plan_work = work.clone();
        plan_work["work"][0]["sourceManagerStepId"] = json!("step-a");
        let leftover_plan = AlwaysOnSnapshot::build(&plan_work, &details, &intents, &runs).unwrap();

        let mut decision_work = work.clone();
        decision_work["work"][0]["kind"] = json!(MANAGER_DECISION_KIND);
        let leftover_decision =
            AlwaysOnSnapshot::build(&decision_work, &details, &intents, &runs).unwrap();

        let stray_intent = AlwaysOnSnapshot::build(
            &work,
            &details,
            &json!({"intents": [{
                "intentId": "intent-stale",
                "workId": SETUP_WORK,
                "attemptId": SETUP_ATTEMPT,
                "runId": "run-elsewhere",
                "inputHash": "hash-stale",
                "workRevision": 1,
                "agentSpecRevision": 1
            }]}),
            &runs,
        )
        .unwrap();

        for (label, polluted) in [
            ("extra Run", polluted_run),
            ("extra Work", polluted_work),
            ("attempt linked off-lane", foreign_link),
            ("leftover plan Work", leftover_plan),
            ("leftover decision Work", leftover_decision),
            ("stray intent", stray_intent),
        ] {
            assert_eq!(
                assert_bootstrap_baseline(&polluted, &fixture, SETUP_RUN),
                Err(DiagnosticCode::StateTransitionMismatch),
                "{label} must not become the accepted baseline"
            );
        }
        // A baseline whose single Run is not the known setup Run is a
        // different session entirely.
        assert_eq!(
            assert_bootstrap_baseline(&baseline(), &fixture, "run-other"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    #[test]
    fn baseline_settling_requires_terminal_states() {
        assert!(baseline_is_settled(&baseline()));
        let mut running = baseline();
        running.runs[0].state = Some("running".into());
        assert!(!baseline_is_settled(&running));
        let mut leased = baseline();
        leased.work[0].state = Some("leased".into());
        assert!(!baseline_is_settled(&leased));
    }

    // -- Home-B pre-restart shape --------------------------------------------

    fn held_projection() -> (Value, BTreeMap<String, Value>, Value, Value) {
        let (mut work, mut details, mut intents, mut runs) = projection_with(&[], true, true);
        work["work"].as_array_mut().unwrap().push(json!({
            "workId": "work-a",
            "kind": "native",
            "sourceManagerStepId": "step-a",
            "state": "running"
        }));
        details.insert(
            "work-a".into(),
            json!({
                "work": {"workId": "work-a"},
                "attempts": [{"attemptId": "attempt-a", "linkedRunIds": ["run-a"]}]
            }),
        );
        intents["intents"].as_array_mut().unwrap().push(json!({
            "intentId": "intent-a",
            "workId": "work-a",
            "attemptId": "attempt-a",
            "runId": "run-a",
            "inputHash": "hash-work-a",
            "workRevision": 1,
            "agentSpecRevision": 1
        }));
        runs["runs"].as_array_mut().unwrap().push(json!({
            "runId": "run-a", "requestId": "intent-a", "purpose": "native", "state": "running"
        }));
        (work, details, intents, runs)
    }

    fn held() -> AlwaysOnSnapshot {
        let (work, details, intents, runs) = held_projection();
        AlwaysOnSnapshot::build(&work, &details, &intents, &runs).expect("held snapshot")
    }

    #[test]
    fn home_b_pre_restart_shape_accepts_exactly_the_held_lane() {
        let fixture = fixture();
        assert_eq!(
            assert_home_b_pre_restart_shape(
                &fixture,
                &baseline(),
                &held(),
                "work-a",
                "attempt-a",
                "run-a"
            ),
            Ok(())
        );
    }

    #[test]
    fn home_b_pre_restart_shape_rejects_extra_preexisting_identities() {
        let fixture = fixture();
        let base = baseline();

        let mut extra_work_json = held_projection();
        extra_work_json.0["work"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "workId": "work-stale", "kind": "native", "state": "succeeded"
            }));
        extra_work_json.1.insert(
            "work-stale".into(),
            json!({
                "work": {"workId": "work-stale"},
                "attempts": [{"attemptId": "attempt-stale", "linkedRunIds": ["run-stale"]}]
            }),
        );
        extra_work_json.3["runs"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "runId": "run-stale", "requestId": "req-stale", "state": "completed"
            }));

        let mut extra_run_json = held_projection();
        extra_run_json.3["runs"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "runId": "run-stale", "requestId": "req-stale", "state": "completed"
            }));

        let mut extra_intent_json = held_projection();
        extra_intent_json.2["intents"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "intentId": "intent-stale",
                "workId": SETUP_WORK,
                "attemptId": SETUP_ATTEMPT,
                "runId": SETUP_RUN,
                "inputHash": "hash-stale",
                "workRevision": 1,
                "agentSpecRevision": 1
            }));

        let mut dependent_json = held_projection();
        dependent_json.0["work"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "workId": "work-b",
                "kind": "native",
                "sourceManagerStepId": "step-b",
                "state": "queued"
            }));
        dependent_json.1.insert(
            "work-b".into(),
            json!({"work": {"workId": "work-b"}, "attempts": []}),
        );

        let mut decision_json = held_projection();
        decision_json.0["work"].as_array_mut().unwrap().push(json!({
            "workId": "work-d",
            "kind": MANAGER_DECISION_KIND,
            "sourceManagerStepId": MANAGER_DECISION_STEP_ID,
            "state": "succeeded"
        }));
        decision_json.1.insert(
            "work-d".into(),
            json!({"work": {"workId": "work-d"}, "attempts": []}),
        );

        let mut proposal_json = held_projection();
        proposal_json.3["runs"].as_array_mut().unwrap().push(json!({
            "runId": "run-d",
            "requestId": "req-d",
            "purpose": MANAGER_PROPOSAL_PURPOSE,
            "state": "completed"
        }));

        for (label, parts) in [
            ("extra preexisting Work", extra_work_json),
            ("extra preexisting Run", extra_run_json),
            ("extra preexisting intent", extra_intent_json),
            ("dependent step already materialised", dependent_json),
            ("manager decision already materialised", decision_json),
            ("proposal Run already present", proposal_json),
        ] {
            let snapshot = AlwaysOnSnapshot::build(&parts.0, &parts.1, &parts.2, &parts.3).unwrap();
            assert_eq!(
                assert_home_b_pre_restart_shape(
                    &fixture,
                    &base,
                    &snapshot,
                    "work-a",
                    "attempt-a",
                    "run-a"
                ),
                Err(DiagnosticCode::StateTransitionMismatch),
                "{label} must fail the pre-restart shape"
            );
        }
        // The held lane must be the join the probe actually captured.
        for (work_id, attempt_id, run_id) in [
            ("work-other", "attempt-a", "run-a"),
            ("work-a", "attempt-other", "run-a"),
            ("work-a", "attempt-a", "run-other"),
        ] {
            assert_eq!(
                assert_home_b_pre_restart_shape(
                    &fixture,
                    &base,
                    &held(),
                    work_id,
                    attempt_id,
                    run_id
                ),
                Err(DiagnosticCode::StateTransitionMismatch)
            );
        }
    }

    // -- happy shape and the manager-decision binding ------------------------

    #[test]
    fn happy_shape_accepts_the_canonical_projection_and_binds_the_decision() {
        let fixture = fixture();
        let shape = assert_happy_shape(&fixture, &baseline(), &happy()).expect("happy shape");
        assert_eq!(shape.native_lanes.len(), 3);
        assert_eq!(shape.decision_lane.step_id, MANAGER_DECISION_STEP_ID);
        assert_eq!(shape.decision_lane.work, opaque_durable_id("work-d"));
        assert_eq!(
            shape.manager_decision_binding,
            ManagerDecisionBinding::Bound {
                lane: AlwaysOnLaneEvidence {
                    step_id: MANAGER_DECISION_STEP_ID.into(),
                    work: opaque_durable_id("work-d"),
                    attempt: opaque_durable_id("attempt-d"),
                    intent: opaque_durable_id("intent-d"),
                    run: opaque_durable_id("run-d"),
                }
            }
        );
        // Published evidence must never carry a raw durable identifier: the
        // report is redaction-scanned before it is written.
        let published = serde_json::to_value(&shape).unwrap();
        grokptah_agent_bridge::scan_value_for_forbidden_data(&published)
            .expect("published always-on shape must survive the redaction scan");
        assert!(!published.to_string().contains("work-d"));
        assert!(shape.manager_decision_binding.is_bound());
        // The counts the identity oracle implies, cross-checked independently.
        assert_eq!(
            expected_happy_cardinality(&fixture, baseline().counts).unwrap(),
            AlwaysOnCardinality {
                work: 6,
                runs: 5,
                intents: 4
            }
        );
        assert_eq!(
            happy().counts,
            AlwaysOnCardinality {
                work: 6,
                runs: 5,
                intents: 4
            }
        );
        assert_eq!(require_plan_work(&happy()).unwrap().work_id, PLAN_WORK);
    }

    /// The shapes below are what the shipped `grokptah-service` actually
    /// projects, recorded so a contract change shows up as a test failure
    /// rather than as a probe that silently stops proving anything.
    #[test]
    fn observed_service_contract_shapes_are_pinned() {
        let fixture = fixture();
        // A direct `ptah_submit_task` materialises one Run and no Work or
        // intents, so the bootstrap baseline is Run-only.
        let bare_baseline = AlwaysOnSnapshot::build(
            &json!({"work": []}),
            &BTreeMap::new(),
            &json!({"intents": []}),
            &json!({"runs": [{
                "runId": SETUP_RUN,
                "requestId": "req-setup",
                "purpose": "execution",
                "state": "completed"
            }]}),
        )
        .unwrap();
        assert_eq!(
            bare_baseline.counts,
            AlwaysOnCardinality {
                work: 0,
                runs: 1,
                intents: 0
            }
        );
        assert_eq!(
            assert_bootstrap_baseline(&bare_baseline, &fixture, SETUP_RUN),
            Ok(())
        );
        // The completed plan adds its own Work row, three native lanes and the
        // manager-decision lane whose Run carries `manager_proposal`.
        assert_eq!(
            expected_happy_cardinality(&fixture, bare_baseline.counts).unwrap(),
            AlwaysOnCardinality {
                work: 5,
                runs: 5,
                intents: 4
            }
        );
        // While the first step is held, only the plan Work and the held lane
        // exist.
        assert_eq!(
            expected_pre_restart_cardinality(&fixture, bare_baseline.counts).unwrap(),
            AlwaysOnCardinality {
                work: 2,
                runs: 2,
                intents: 1
            }
        );
    }

    #[test]
    fn happy_shape_pins_each_step_to_its_fixture_terminal_state() {
        let fixture = fixture();
        assert_eq!(expected_step_state(&fixture, "step-a"), Ok("succeeded"));
        assert_eq!(expected_step_state(&fixture, "step-b"), Ok("failed"));
        assert_eq!(expected_step_state(&fixture, "step-b-fix"), Ok("succeeded"));
        assert_eq!(
            expected_step_state(&fixture, MANAGER_DECISION_STEP_ID),
            Ok("succeeded")
        );
        assert_eq!(
            expected_step_state(&fixture, "step-unknown"),
            Err(DiagnosticCode::FixtureInvalid)
        );
        // Each step's terminal state is part of the oracle: a forced-failure
        // step that reports success, or a step that should succeed reporting
        // failure, must both be rejected.
        for (index, state) in [(3usize, "succeeded"), (2, "failed"), (4, "failed")] {
            let mut parts = projection(&happy_lanes(), true);
            parts.0["work"][index]["state"] = json!(state);
            let snapshot = AlwaysOnSnapshot::build(&parts.0, &parts.1, &parts.2, &parts.3).unwrap();
            assert_eq!(
                assert_happy_shape(&fixture, &baseline(), &snapshot),
                Err(DiagnosticCode::StateTransitionMismatch),
                "work row {index} reporting {state} must fail"
            );
        }
        // The manager-decision lane must have succeeded.
        let mut decision = projection(&happy_lanes(), true);
        decision.0["work"][5]["state"] = json!("failed");
        let decision =
            AlwaysOnSnapshot::build(&decision.0, &decision.1, &decision.2, &decision.3).unwrap();
        assert_eq!(
            assert_happy_shape(&fixture, &baseline(), &decision),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    #[test]
    fn happy_shape_rejects_misbound_decision_and_proposal() {
        let fixture = fixture();
        let base = baseline();

        // The decision Work's own Run is not the proposal Run; some other Run
        // carries the purpose. Counting "one decision Work" and "one proposal
        // Run" separately still passes, so only the join catches this.
        let mut misbound = projection(&happy_lanes(), true);
        misbound.3["runs"][4]["purpose"] = json!("native");
        misbound.3["runs"][1]["purpose"] = json!(MANAGER_PROPOSAL_PURPOSE);
        let misbound =
            AlwaysOnSnapshot::build(&misbound.0, &misbound.1, &misbound.2, &misbound.3).unwrap();
        assert_eq!(misbound.counts, happy().counts);
        assert_eq!(
            misbound
                .work
                .iter()
                .filter(|item| item.kind.as_deref() == Some(MANAGER_DECISION_KIND))
                .count(),
            1,
            "the decision Work count is unchanged, so counting cannot catch this"
        );
        assert_eq!(
            misbound.runs_with_purpose(MANAGER_PROPOSAL_PURPOSE).len(),
            1,
            "the proposal Run count is unchanged, so counting cannot catch this"
        );
        assert_eq!(
            assert_happy_shape(&fixture, &base, &misbound),
            Err(DiagnosticCode::StateTransitionMismatch)
        );

        // Two proposal Runs must not satisfy the binding either.
        let mut doubled = projection(&happy_lanes(), true);
        doubled.3["runs"][1]["purpose"] = json!(MANAGER_PROPOSAL_PURPOSE);
        let doubled =
            AlwaysOnSnapshot::build(&doubled.0, &doubled.1, &doubled.2, &doubled.3).unwrap();
        assert_eq!(
            assert_happy_shape(&fixture, &base, &doubled),
            Err(DiagnosticCode::StateTransitionMismatch)
        );

        // A decision Work whose public kind does not agree with its reserved
        // step id is not the decision lane.
        let mut mislabelled = projection(&happy_lanes(), true);
        mislabelled.0["work"][5]["kind"] = json!("native");
        let mislabelled = AlwaysOnSnapshot::build(
            &mislabelled.0,
            &mislabelled.1,
            &mislabelled.2,
            &mislabelled.3,
        )
        .unwrap();
        assert_eq!(
            assert_happy_shape(&fixture, &base, &mislabelled),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    #[test]
    fn manager_decision_binding_records_an_unprojected_purpose_as_a_boundary() {
        // If the public contract ever stops projecting `purpose`, the join
        // cannot be made. The boundary is recorded explicitly and is never
        // reported as a proven causal oracle.
        let mut parts = projection(&happy_lanes(), true);
        for run in parts.3["runs"].as_array_mut().unwrap() {
            run.as_object_mut().unwrap().remove("purpose");
        }
        let snapshot = AlwaysOnSnapshot::build(&parts.0, &parts.1, &parts.2, &parts.3).unwrap();
        let (lane, binding) = bind_manager_decision(&snapshot).expect("lane resolves");
        assert_eq!(lane.work_id, "work-d");
        assert_eq!(
            binding,
            ManagerDecisionBinding::PurposeNotProjected {
                work: opaque_durable_id("work-d")
            }
        );
        assert!(!binding.is_bound());
        // The probe must not pass on the boundary: the fixture declares one
        // observed proposal Run, which this projection cannot prove.
        assert_eq!(
            assert_happy_shape(&fixture(), &baseline(), &snapshot),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    #[test]
    fn happy_shape_rejects_missing_lanes_and_residual_growth() {
        let fixture = fixture();
        let base = baseline();

        // A missing replacement lane.
        let lanes = happy_lanes();
        let trimmed: Vec<Lane> = lanes
            .into_iter()
            .filter(|lane| lane.step != "step-b-fix")
            .collect();
        assert_eq!(
            assert_happy_shape(&fixture, &base, &snapshot(&trimmed, true)),
            Err(DiagnosticCode::StateTransitionMismatch)
        );

        // A duplicated step lane.
        let mut duplicated = projection(&happy_lanes(), true);
        duplicated.0["work"].as_array_mut().unwrap().push(json!({
            "workId": "work-a2",
            "kind": "native",
            "sourceManagerStepId": "step-a",
            "state": "succeeded"
        }));
        duplicated.1.insert(
            "work-a2".into(),
            json!({"work": {"workId": "work-a2"}, "attempts": []}),
        );
        let duplicated =
            AlwaysOnSnapshot::build(&duplicated.0, &duplicated.1, &duplicated.2, &duplicated.3)
                .unwrap();
        assert_eq!(
            assert_happy_shape(&fixture, &base, &duplicated),
            Err(DiagnosticCode::StateTransitionMismatch)
        );

        // Residual growth outside every plan lane.
        let mut residual_growth = projection(&happy_lanes(), true);
        residual_growth.3["runs"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "runId": "run-extra", "requestId": "req-extra", "state": "completed"
            }));
        let residual_growth = AlwaysOnSnapshot::build(
            &residual_growth.0,
            &residual_growth.1,
            &residual_growth.2,
            &residual_growth.3,
        )
        .unwrap();
        assert_eq!(
            assert_happy_shape(&fixture, &base, &residual_growth),
            Err(DiagnosticCode::StateTransitionMismatch)
        );

        // A baseline the final snapshot no longer contains byte for byte.
        let mut drifted = base.clone();
        drifted.work[0].state = Some("failed".into());
        assert_eq!(
            assert_happy_shape(&fixture, &drifted, &happy()),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    // -- post-restart steady state -------------------------------------------

    /// The held home after a restart: the held lane failed over an interrupted
    /// Run, plus the manager's single decision lane.
    fn post_restart() -> AlwaysOnSnapshot {
        let (mut work, mut details, mut intents, mut runs) = held_projection();
        work["work"][2]["state"] = json!("failed");
        for run in runs["runs"].as_array_mut().unwrap() {
            if run["runId"] == json!("run-a") {
                run["state"] = json!("interrupted");
            }
        }
        work["work"].as_array_mut().unwrap().push(json!({
            "workId": "work-d",
            "kind": MANAGER_DECISION_KIND,
            "sourceManagerStepId": MANAGER_DECISION_STEP_ID,
            "state": "succeeded"
        }));
        details.insert(
            "work-d".into(),
            json!({
                "work": {"workId": "work-d"},
                "attempts": [{"attemptId": "attempt-d", "linkedRunIds": ["run-d"]}]
            }),
        );
        intents["intents"].as_array_mut().unwrap().push(json!({
            "intentId": "intent-d",
            "workId": "work-d",
            "attemptId": "attempt-d",
            "runId": "run-d",
            "inputHash": "hash-work-d",
            "workRevision": 1,
            "agentSpecRevision": 1
        }));
        runs["runs"].as_array_mut().unwrap().push(json!({
            "runId": "run-d",
            "requestId": "intent-d",
            "purpose": MANAGER_PROPOSAL_PURPOSE,
            "state": "completed"
        }));
        AlwaysOnSnapshot::build(&work, &details, &intents, &runs).expect("post-restart snapshot")
    }

    #[test]
    fn post_restart_shape_accepts_exactly_one_manager_reaction() {
        let lane = assert_post_restart_shape(&held(), &post_restart(), "work-a", "run-a")
            .expect("post-restart steady state");
        assert_eq!(lane.work_id, "work-d");
        assert_eq!(lane.run_id, "run-d");
        // Comparing pre-restart against post-restart directly can never hold:
        // the manager legitimately adds its decision lane.
        assert_eq!(
            assert_exact_snapshot(
                &held().with_interruption("work-a", "run-a"),
                &post_restart()
            ),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    #[test]
    fn post_restart_shape_rejects_any_movement_beyond_the_decision_lane() {
        // The held lane must land on failed Work over an interrupted Run.
        let mut still_running = post_restart();
        still_running
            .runs
            .iter_mut()
            .find(|run| run.run_id == "run-a")
            .unwrap()
            .state = Some("running".into());
        assert_eq!(
            assert_post_restart_shape(&held(), &still_running, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        let mut not_failed = post_restart();
        not_failed
            .work
            .iter_mut()
            .find(|item| item.work_id == "work-a")
            .unwrap()
            .state = Some("succeeded".into());
        assert_eq!(
            assert_post_restart_shape(&held(), &not_failed, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // The held lane's identities must survive the restart untouched.
        let mut relinked = post_restart();
        relinked
            .work
            .iter_mut()
            .find(|item| item.work_id == "work-a")
            .unwrap()
            .attempts[0]
            .linked_run_ids = vec!["run-readmitted".into()];
        assert_eq!(
            assert_post_restart_shape(&held(), &relinked, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // Growth beyond the single decision lane must fail.
        let mut grown = post_restart();
        grown.runs.push(RunIdentity {
            run_id: "run-extra".into(),
            request_id: "req-extra".into(),
            purpose: Some("execution".into()),
            state: Some("running".into()),
        });
        grown.runs.sort();
        grown.counts.runs += 1;
        assert_eq!(
            assert_post_restart_shape(&held(), &grown, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // The plan Work row must still be there and still be attempt-free.
        let mut plan_gone = post_restart();
        plan_gone.work.retain(|item| item.work_id != PLAN_WORK);
        plan_gone.counts.work -= 1;
        assert_eq!(
            assert_post_restart_shape(&held(), &plan_gone, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        assert_eq!(
            require_plan_work(&plan_gone),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        let mut plan_with_attempt = post_restart();
        plan_with_attempt
            .work
            .iter_mut()
            .find(|item| item.work_id == PLAN_WORK)
            .unwrap()
            .attempts
            .push(AttemptIdentity {
                attempt_id: "attempt-plan".into(),
                linked_run_ids: vec!["run-plan".into()],
            });
        assert_eq!(
            require_plan_work(&plan_with_attempt),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // Two decision lanes are two reactions, not one.
        let mut doubled = post_restart();
        let mut second = doubled
            .work
            .iter()
            .find(|item| item.work_id == "work-d")
            .unwrap()
            .clone();
        second.work_id = "work-d2".into();
        doubled.work.push(second);
        doubled.work.sort();
        doubled.counts.work += 1;
        assert_eq!(
            assert_post_restart_shape(&held(), &doubled, "work-a", "run-a"),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    // -- loopback provider lanes ---------------------------------------------

    fn record(semantic: &str, digest: &str) -> LoopbackProviderRecord {
        LoopbackProviderRecord {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            semantic_id: semantic.into(),
            body_digest: digest.into(),
            auth_accepted: true,
            route_ok: true,
        }
    }

    fn home_a_lane() -> LoopbackProviderLane {
        LoopbackProviderLane {
            home: AlwaysOnHome::HomeA,
            accepted_posts: 5,
            rejected_auth: 0,
            records: vec![
                record(SETUP_SEMANTIC_ID, "d-setup-a"),
                record("step-a", "d-step-a"),
                record("step-b", "d-step-b"),
                record(MANAGER_DECISION_KIND, "d-decision"),
                record("step-b-fix", "d-step-b-fix"),
            ],
        }
    }

    fn home_b_lane() -> LoopbackProviderLane {
        LoopbackProviderLane {
            home: AlwaysOnHome::HomeB,
            accepted_posts: 3,
            rejected_auth: 0,
            records: vec![
                record(SETUP_SEMANTIC_ID, "d-setup-b"),
                record("step-a", "d-step-a-held"),
                // The manager's single reaction to the held step failing.
                record(MANAGER_DECISION_KIND, "d-decision-b"),
            ],
        }
    }

    #[test]
    fn provider_lanes_accept_both_homes_at_the_exact_fixture_counts() {
        let fixture = fixture();
        let lanes = vec![home_a_lane(), home_b_lane()];
        assert_eq!(assert_provider_lanes(&fixture, &lanes), Ok(()));
        let merged = merge_provider_lanes(&lanes).expect("merged observation");
        assert_eq!(merged.accepted_posts, 8);
        assert_eq!(merged.rejected_auth, 0);
        assert_eq!(merged.records.len(), 8);
        // Home A's records come first and stay reconstructable.
        assert_eq!(merged.records[0].body_digest, "d-setup-a");
        assert_eq!(merged.records[5].body_digest, "d-setup-b");
        assert_eq!(merge_provider_lanes(&[]), None);
        // Lane order in the input does not change the merged evidence.
        assert_eq!(
            merge_provider_lanes(&[home_b_lane(), home_a_lane()]),
            Some(merged)
        );
    }

    #[test]
    fn provider_lanes_reject_missing_duplicate_and_miscounted_records() {
        let fixture = fixture();
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_a_lane()]),
            Err(DiagnosticCode::ProviderObservationDropped),
            "a report without Home B must not pass"
        );
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_b_lane()]),
            Err(DiagnosticCode::ProviderObservationDropped),
            "a pass claiming Home-A oracles must carry Home-A records"
        );
        assert_eq!(
            assert_provider_lanes(&fixture, &[]),
            Err(DiagnosticCode::ProviderObservationDropped)
        );
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_a_lane(), home_a_lane(), home_b_lane()]),
            Err(DiagnosticCode::ProviderObservationDropped),
            "a duplicated home lane is ambiguous evidence"
        );
        let mut empty_a = home_a_lane();
        empty_a.records.clear();
        assert_eq!(
            assert_provider_lanes(&fixture, &[empty_a, home_b_lane()]),
            Err(DiagnosticCode::ProviderObservationDropped)
        );

        // Home A missing one declared semantic id.
        for semantic in ["step-a", "step-b", "step-b-fix", MANAGER_DECISION_KIND] {
            let mut lane = home_a_lane();
            lane.records.retain(|item| item.semantic_id != semantic);
            lane.accepted_posts -= 1;
            assert_eq!(
                assert_provider_lanes(&fixture, &[lane, home_b_lane()]),
                Err(DiagnosticCode::StateTransitionMismatch),
                "Home A missing {semantic} must fail"
            );
        }
        // Home A duplicating a declared semantic id.
        let mut duplicated = home_a_lane();
        duplicated.records.push(record("step-a", "d-step-a-again"));
        duplicated.accepted_posts += 1;
        assert_eq!(
            assert_provider_lanes(&fixture, &[duplicated, home_b_lane()]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // Home A missing its bootstrap POST.
        let mut no_setup = home_a_lane();
        no_setup
            .records
            .retain(|item| item.semantic_id != SETUP_SEMANTIC_ID);
        no_setup.accepted_posts -= 1;
        assert_eq!(
            assert_provider_lanes(&fixture, &[no_setup, home_b_lane()]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // A record the provider rejected does not count as evidence.
        let mut rejected = home_a_lane();
        rejected.records[1].auth_accepted = false;
        assert_eq!(
            assert_provider_lanes(&fixture, &[rejected, home_b_lane()]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        let mut misrouted = home_a_lane();
        misrouted.records[1].route_ok = false;
        assert_eq!(
            assert_provider_lanes(&fixture, &[misrouted, home_b_lane()]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // Home B must never have reached the later lanes.
        for semantic in ["step-b", "step-b-fix", MANAGER_DECISION_KIND] {
            let mut lane = home_b_lane();
            lane.records.push(record(semantic, "d-leaked"));
            lane.accepted_posts += 1;
            assert_eq!(
                assert_provider_lanes(&fixture, &[home_a_lane(), lane]),
                Err(DiagnosticCode::StateTransitionMismatch),
                "Home B reaching {semantic} again must fail"
            );
        }
        // Home B must show exactly one manager-decision POST: the manager
        // reacts once, and neither restart may re-drive it.
        let mut no_decision = home_b_lane();
        no_decision
            .records
            .retain(|item| item.semantic_id != MANAGER_DECISION_KIND);
        no_decision.accepted_posts -= 1;
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_a_lane(), no_decision]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        // Home B missing or duplicating its held step.
        let mut missing_held = home_b_lane();
        missing_held
            .records
            .retain(|item| item.semantic_id != "step-a");
        missing_held.accepted_posts -= 1;
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_a_lane(), missing_held]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
        let mut resent = home_b_lane();
        resent.records.push(record("step-a", "d-step-a-resent"));
        resent.accepted_posts += 1;
        assert_eq!(
            assert_provider_lanes(&fixture, &[home_a_lane(), resent]),
            Err(DiagnosticCode::StateTransitionMismatch),
            "a resent held POST after restart must fail"
        );
        // The declared totals must agree with the records.
        let mut inflated = home_a_lane();
        inflated.accepted_posts = 6;
        assert_eq!(
            assert_provider_lanes(&fixture, &[inflated, home_b_lane()]),
            Err(DiagnosticCode::StateTransitionMismatch)
        );
    }

    // -- fixture parser -----------------------------------------------------

    #[test]
    fn canonical_fixture_parses_and_drives_the_runtime_oracle() {
        let fixture = fixture();
        assert_eq!(fixture.schema_version, 2);
        assert_eq!(fixture.step_first, "step-a");
        assert_eq!(fixture.step_failing, "step-b");
        assert_eq!(fixture.step_replacement, "step-b-fix");
        assert_eq!(fixture.decision_work, 1);
        assert_eq!(fixture.proposal_runs, 1);
        assert_eq!(fixture.zero_growth_window, Duration::from_millis(4000));
        assert_eq!(fixture.expected_total_posts().unwrap(), 5);
    }

    #[test]
    fn fixture_parser_rejects_unknown_fields_at_every_level() {
        for pointer in [
            vec![],
            vec!["sentinels"],
            vec!["steps"],
            vec!["happyPath"],
            vec!["resourceCeilings"],
            vec!["artifactScan"],
        ] {
            let mut mutant = canonical_value();
            let mut cursor = &mut mutant;
            for key in &pointer {
                cursor = cursor.get_mut(key).expect("section");
            }
            cursor["unexpectedKey"] = json!("smuggled");
            let error = AlwaysOnFixture::from_value(mutant).expect_err("unknown key must fail");
            assert!(error.contains("unknown keys"), "{pointer:?}: {error}");
        }
    }

    #[test]
    fn fixture_parser_rejects_a_removed_key_it_never_reads() {
        // `seed` is not consulted by any oracle; dropping it must still fail so
        // the fixture cannot quietly shed the fields that document its claim.
        let mut mutant = canonical_value();
        mutant.as_object_mut().unwrap().remove("seed");
        assert!(AlwaysOnFixture::from_value(mutant)
            .expect_err("missing key")
            .contains("missing seed"));
    }

    #[test]
    fn fixture_parser_rejects_duplicate_object_keys() {
        let raw = br#"{"schema":"x","schema":"y"}"#;
        assert!(parse_duplicate_free(raw)
            .expect_err("duplicate key")
            .contains("duplicate object key"));
        let text = String::from_utf8(crate::ALWAYS_ON_GROKBOT_FIXTURE.to_vec()).unwrap();
        let duplicated = text.replacen(
            "\"decisionWork\": 1,",
            "\"decisionWork\": 1,\n      \"decisionWork\": 9,",
            1,
        );
        assert!(AlwaysOnFixture::parse(duplicated.as_bytes())
            .expect_err("duplicate happyPath key")
            .contains("duplicate object key"));
    }

    #[test]
    fn fixture_parser_rejects_duplicate_required_assertions() {
        let mut mutant = canonical_value();
        let first = mutant["requiredAssertions"][0].clone();
        mutant["requiredAssertions"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(AlwaysOnFixture::from_value(mutant)
            .expect_err("duplicate assertion")
            .contains("unique"));
    }

    #[test]
    fn fixture_parser_rejects_invalid_and_extra_step_mappings() {
        // A step renamed without updating its count maps.
        let mut renamed = canonical_value();
        renamed["steps"]["replacement"] = json!("step-z");
        assert!(AlwaysOnFixture::from_value(renamed).is_err());
        // An extra semantic id in the native map.
        let mut extra_native = canonical_value();
        extra_native["happyPath"]["nativeWorkByStep"]["step-extra"] = json!(1);
        assert!(AlwaysOnFixture::from_value(extra_native).is_err());
        // An extra semantic id in the provider map.
        let mut extra_posts = canonical_value();
        extra_posts["happyPath"]["providerPostsBySemanticId"]["step-extra"] = json!(1);
        assert!(AlwaysOnFixture::from_value(extra_posts).is_err());
        // A missing mapping.
        let mut missing = canonical_value();
        missing["happyPath"]["nativeWorkByStep"]
            .as_object_mut()
            .unwrap()
            .remove("step-b-fix");
        assert!(AlwaysOnFixture::from_value(missing).is_err());
        // A count that is not exactly one.
        for section in ["nativeWorkByStep", "providerPostsBySemanticId"] {
            let mut inflated = canonical_value();
            inflated["happyPath"][section]["step-a"] = json!(2);
            assert!(AlwaysOnFixture::from_value(inflated).is_err(), "{section}");
        }
    }

    #[test]
    fn fixture_parser_rejects_inconsistent_semantic_ids() {
        // Two steps sharing an id would make the per-step oracles ambiguous.
        let mut collided = canonical_value();
        collided["steps"]["failing"] = json!("step-a");
        assert!(AlwaysOnFixture::from_value(collided)
            .expect_err("duplicate step ids")
            .contains("distinct"));
        // A step id colliding with a reserved semantic id would let the
        // manager-decision lane masquerade as a native step.
        for reserved in RESERVED_SEMANTIC_IDS {
            let mut mutant = canonical_value();
            mutant["steps"]["first"] = json!(reserved);
            mutant["happyPath"]["nativeWorkByStep"] = json!({
                *reserved: 1, "step-b": 1, "step-b-fix": 1
            });
            mutant["happyPath"]["providerPostsBySemanticId"] = json!({
                *reserved: 1, "step-b": 1, "step-b-fix": 1, MANAGER_DECISION_KIND: 1
            });
            assert!(
                AlwaysOnFixture::from_value(mutant).is_err(),
                "reserved id {reserved} must not be a step id"
            );
        }
    }

    #[test]
    fn fixture_parser_rejects_zero_and_overflowing_windows() {
        for (key, value) in [
            ("supervisorPeriodMs", json!(0)),
            ("zeroGrowthSupervisorPeriods", json!(0)),
        ] {
            let mut mutant = canonical_value();
            mutant[key] = value;
            assert!(
                AlwaysOnFixture::from_value(mutant)
                    .expect_err("zero window")
                    .contains("greater than zero"),
                "{key}"
            );
        }
        let mut overflow = canonical_value();
        overflow["supervisorPeriodMs"] = json!(u64::MAX);
        overflow["zeroGrowthSupervisorPeriods"] = json!(2);
        assert!(AlwaysOnFixture::from_value(overflow)
            .expect_err("overflowing window")
            .contains("overflows"));
        // Negative and fractional values are not u64 windows at all.
        for value in [json!(-1), json!(1.5)] {
            let mut mutant = canonical_value();
            mutant["supervisorPeriodMs"] = value;
            assert!(AlwaysOnFixture::from_value(mutant).is_err());
        }
    }

    #[test]
    fn fixture_parser_rejects_zero_ceilings_and_scan_budgets() {
        let mut ceiling = canonical_value();
        ceiling["resourceCeilings"]["maxRssBytes"] = json!(0);
        assert!(AlwaysOnFixture::from_value(ceiling).is_err());
        let mut scan = canonical_value();
        scan["artifactScan"]["maxFiles"] = json!(0);
        assert!(AlwaysOnFixture::from_value(scan).is_err());
    }

    #[test]
    fn fixture_parser_rejects_schema_and_version_drift() {
        let mut schema = canonical_value();
        schema["schema"] = json!("grokptah.some_other_fixture.v1");
        assert!(AlwaysOnFixture::from_value(schema).is_err());
        let mut version = canonical_value();
        version["schemaVersion"] = json!(3);
        assert!(AlwaysOnFixture::from_value(version).is_err());
    }

    #[test]
    fn fixture_parser_rejects_incomplete_fail_closed_matrix() {
        let mut missing = canonical_value();
        missing["failClosed"]
            .as_object_mut()
            .unwrap()
            .remove("slow");
        assert!(AlwaysOnFixture::from_value(missing).is_err());
        let mut extra = canonical_value();
        extra["failClosed"]["unexpected"] = json!({
            "runState": "x", "stopCause": "y", "errorCode": "z", "posts": 1
        });
        assert!(AlwaysOnFixture::from_value(extra).is_err());
        let mut posts = canonical_value();
        posts["failClosed"]["cancel"]["posts"] = json!(2);
        assert!(AlwaysOnFixture::from_value(posts).is_err());
    }
}
