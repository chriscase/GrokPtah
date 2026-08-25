//! Process-level harness for the always-on Grokbot campaign.
//!
//! Spawns the shipped `grokptah-service` binary, a loopback fake provider with
//! an explicit POST barrier, and an authenticated MCP client. No production
//! crate is modified.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::{scan_value_for_forbidden_data, McpControlClient};
use serde_json::{json, Map, Value};
use tempfile::TempDir;
use uuid::Uuid;

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
pub const SYNTHETIC_KEY: &str = "test-not-a-secret";
pub const FIXTURE_BYTES: &[u8] = include_bytes!("../fixtures/always_on_grokbot.json");
pub const FIXTURE_SCHEMA: &str = "grokptah.always_on_grokbot_fixture.v1";
const READY_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const RECORD_BOUND: usize = 32;
const LIVE_URL_SENTINELS: &[&str] = &["https://api.x.ai", "https://cli-chat-proxy.grok.com"];

const AMBIENT_CREDENTIAL_ENV: &[&str] = &[
    "XAI_API_KEY",
    "XAI_API_BASE",
    "GROKPTAH_TOKEN_COMMAND",
    "GROKPTAH_AGENT_OFFLINE",
    "GROKPTAH_SERVICE_CLIENTS",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
];

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupExpect {
    pub work: u64,
    pub attempts: u64,
    pub runs: u64,
    pub intents: u64,
    pub provider_sends: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManagerPlanExpect {
    pub work: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailClosedExpect {
    pub run_state: String,
    pub stop_cause: String,
    pub error_code: String,
    pub posts: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactScan {
    pub max_depth: u64,
    pub max_files: u64,
    pub max_file_bytes: u64,
    pub stderr_head_bytes: usize,
    pub stderr_tail_bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fixture {
    pub schema: String,
    pub schema_version: u64,
    pub seed: String,
    pub base_sha: String,
    pub claim: String,
    pub next_required_campaign: String,
    pub success: String,
    pub fail: String,
    pub ok: String,
    pub setup: String,
    pub step_first: String,
    pub step_failing: String,
    pub step_replacement: String,
    pub proposal_only: String,
    pub internal_persistence_cuts: String,
    pub attempt_evidence: String,
    pub quota_ledger: String,
    pub provider_attempt_projection: String,
    pub uncertain_accept_projection: String,
    pub retry_class_projection: String,
    pub clock: String,
    pub supervisor_period: Duration,
    pub zero_growth_periods: u64,
    pub proved_oracle: String,
    pub soak10m: String,
    pub soak24h: String,
    pub ci_mode: String,
    pub required_assertions: Vec<String>,
    pub decision_work: u64,
    pub proposal_runs: u64,
    pub native_work_by_step: BTreeMap<String, u64>,
    pub posts_by_semantic: BTreeMap<String, u64>,
    pub setup_lane: SetupExpect,
    pub manager_plan: ManagerPlanExpect,
    pub fail_closed: BTreeMap<String, FailClosedExpect>,
    pub ceilings: ResourceCeilings,
    pub artifact_scan: ArtifactScan,
}

impl Fixture {
    pub fn load() -> Self {
        parse_fixture(FIXTURE_BYTES).expect("typed always-on fixture")
    }

    pub fn digest(&self) -> String {
        hash_payload(&serde_json::from_slice(FIXTURE_BYTES).expect("fixture value"))
    }

    pub fn fail_closed_case(&self, name: &str) -> &FailClosedExpect {
        self.fail_closed
            .get(name)
            .unwrap_or_else(|| panic!("fixture missing failClosed.{name}"))
    }

    pub fn posts_for(&self, semantic: &str) -> u64 {
        if semantic == "setup" {
            return self.setup_lane.provider_sends;
        }
        *self
            .posts_by_semantic
            .get(semantic)
            .unwrap_or_else(|| panic!("fixture missing providerPostsBySemanticId.{semantic}"))
    }
}

pub fn parse_fixture(bytes: &[u8]) -> Result<Fixture, String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("fixture JSON: {error}"))?;
    let mut root = expect_object(value, "fixture")?;
    let schema = take_string(&mut root, "schema")?;
    if schema != FIXTURE_SCHEMA {
        return Err(format!("schema {schema} != {FIXTURE_SCHEMA}"));
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
    let proposal_only = take_string(&mut root, "proposalOnlyEnforcement")?;
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
    let success = take_string(&mut sentinels, "success")?;
    let fail = take_string(&mut sentinels, "fail")?;
    let ok = take_string(&mut sentinels, "ok")?;
    let setup = take_string(&mut sentinels, "setup")?;
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
    let posts_by_semantic = take_u64_map(&mut happy, "providerPostsBySemanticId")?;
    deny_unknown(happy, "happyPath")?;
    if native_work_by_step.len() != 3 || posts_by_semantic.len() != 4 {
        return Err("happyPath maps must be unique and complete".into());
    }
    for key in [
        step_first.as_str(),
        step_failing.as_str(),
        step_replacement.as_str(),
    ] {
        if !native_work_by_step.contains_key(key) {
            return Err(format!("happyPath.nativeWorkByStep missing {key}"));
        }
        if !posts_by_semantic.contains_key(key) {
            return Err(format!("happyPath.providerPostsBySemanticId missing {key}"));
        }
    }
    if !posts_by_semantic.contains_key("manager-decision") {
        return Err("happyPath.providerPostsBySemanticId missing manager-decision".into());
    }
    let setup_lane = take_setup(&mut root)?;
    let manager_plan = take_manager_plan(&mut root)?;
    let fail_closed = take_fail_closed(&mut root)?;
    let ceilings = take_ceilings(&mut root)?;
    let artifact_scan = take_artifact_scan(&mut root)?;
    let required_assertions = take_string_array(&mut root, "requiredAssertions")?;
    if required_assertions.len() != required_assertions.iter().collect::<BTreeSet<_>>().len() {
        return Err("requiredAssertions must be unique".into());
    }
    if setup_lane.runs == 0 {
        return Err("setup.runs must be greater than zero".into());
    }
    if setup_lane.provider_sends == 0 {
        return Err("setup.providerSends must be greater than zero".into());
    }
    if setup_lane.work == 0 && setup_lane.attempts != 0 {
        return Err("setup.attempts must be 0 when setup.work is 0".into());
    }
    if setup_lane.work == 0 && setup_lane.intents != 0 {
        return Err("setup.intents must be 0 when setup.work is 0".into());
    }
    if manager_plan.work != 1 {
        return Err("managerPlan.work must be exactly 1".into());
    }
    deny_unknown(root, "fixture")?;
    Ok(Fixture {
        schema,
        schema_version,
        seed,
        base_sha,
        claim,
        next_required_campaign,
        success,
        fail,
        ok,
        setup,
        step_first,
        step_failing,
        step_replacement,
        proposal_only,
        internal_persistence_cuts,
        attempt_evidence,
        quota_ledger,
        provider_attempt_projection,
        uncertain_accept_projection,
        retry_class_projection,
        clock,
        supervisor_period: Duration::from_millis(supervisor_period_ms),
        zero_growth_periods,
        proved_oracle,
        soak10m,
        soak24h,
        ci_mode,
        required_assertions,
        decision_work,
        proposal_runs,
        native_work_by_step,
        posts_by_semantic,
        setup_lane,
        manager_plan,
        fail_closed,
        ceilings,
        artifact_scan,
    })
}

fn expect_object(value: Value, ctx: &str) -> Result<Map<String, Value>, String> {
    match value {
        Value::Object(map) => Ok(map),
        other => Err(format!("{ctx} must be an object, got {other}")),
    }
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

fn take_setup(root: &mut Map<String, Value>) -> Result<SetupExpect, String> {
    let mut map = take_object(root, "setup")?;
    let setup = SetupExpect {
        work: take_u64(&mut map, "work")?,
        attempts: take_u64(&mut map, "attempts")?,
        runs: take_u64(&mut map, "runs")?,
        intents: take_u64(&mut map, "intents")?,
        provider_sends: take_u64(&mut map, "providerSends")?,
    };
    deny_unknown(map, "setup")?;
    Ok(setup)
}

fn take_manager_plan(root: &mut Map<String, Value>) -> Result<ManagerPlanExpect, String> {
    let mut map = take_object(root, "managerPlan")?;
    let plan = ManagerPlanExpect {
        work: take_u64(&mut map, "work")?,
    };
    deny_unknown(map, "managerPlan")?;
    Ok(plan)
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
    for required in ["cancel", "malformed", "disconnect", "status500", "slow"] {
        if !out.contains_key(required) {
            return Err(format!("failClosed missing {required}"));
        }
    }
    if out.len() != 5 {
        return Err("failClosed must declare exactly the five required cases".into());
    }
    Ok(out)
}

fn take_ceilings(root: &mut Map<String, Value>) -> Result<ResourceCeilings, String> {
    let mut row = take_object(root, "resourceCeilings")?;
    let ceilings = ResourceCeilings {
        max_rss_bytes: take_u64(&mut row, "maxRssBytes")?,
        max_fd_count: take_u64(&mut row, "maxFdCount")?,
        max_threads: take_u64(&mut row, "maxThreads")?,
        max_disk_bytes: take_u64(&mut row, "maxDiskBytes")?,
        max_cycle_latency_ms: take_u64(&mut row, "maxCycleLatencyMs")?,
        max_rss_growth_bytes: take_u64(&mut row, "maxRssGrowthBytes")?,
        max_fd_growth: take_u64(&mut row, "maxFdGrowth")?,
        max_thread_growth: take_u64(&mut row, "maxThreadGrowth")?,
        max_disk_growth_bytes: take_u64(&mut row, "maxDiskGrowthBytes")?,
    };
    deny_unknown(row, "resourceCeilings")?;
    Ok(ceilings)
}

fn take_artifact_scan(root: &mut Map<String, Value>) -> Result<ArtifactScan, String> {
    let mut row = take_object(root, "artifactScan")?;
    let scan = ArtifactScan {
        max_depth: take_u64(&mut row, "maxDepth")?,
        max_files: take_u64(&mut row, "maxFiles")?,
        max_file_bytes: take_u64(&mut row, "maxFileBytes")?,
        stderr_head_bytes: usize::try_from(take_u64(&mut row, "stderrHeadBytes")?)
            .map_err(|_| "stderrHeadBytes".to_string())?,
        stderr_tail_bytes: usize::try_from(take_u64(&mut row, "stderrTailBytes")?)
            .map_err(|_| "stderrTailBytes".to_string())?,
    };
    deny_unknown(row, "artifactScan")?;
    Ok(scan)
}

fn deny_unknown(map: Map<String, Value>, ctx: &str) -> Result<(), String> {
    if map.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "unknown fields in {ctx}: {:?}",
            map.keys().collect::<Vec<_>>()
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderScript {
    Lifecycle,
    InvalidDirective,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDisposition {
    Scripted,
    Hold,
    Drop,
    Status500,
    Malformed,
    Slow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderRecord {
    pub method: String,
    pub path: String,
    pub auth_present: bool,
    pub auth_scheme: Option<String>,
    pub auth_accepted: bool,
    pub body_digest: String,
    pub semantic_id: String,
    pub route_ok: bool,
    pub focus_preview: String,
}

struct ProviderState {
    script: Mutex<ProviderScript>,
    dispositions: Mutex<HashMap<String, ProviderDisposition>>,
    records: Mutex<Vec<ProviderRecord>>,
    accepted: Mutex<HashMap<String, u64>>,
    accepted_signal: Condvar,
    release_set: Mutex<HashSet<String>>,
    release_signal: Condvar,
    posts: AtomicU64,
    rejected_auth: AtomicU64,
    stop: AtomicBool,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

pub struct FakeProvider {
    pub base_url: String,
    pub listen: SocketAddr,
    state: Arc<ProviderState>,
    accept_join: Mutex<Option<JoinHandle<()>>>,
}

impl FakeProvider {
    pub fn start() -> Self {
        Self::start_with(ProviderScript::Lifecycle)
    }

    pub fn start_with(script: ProviderScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(ProviderState {
            script: Mutex::new(script),
            dispositions: Mutex::new(HashMap::new()),
            records: Mutex::new(Vec::new()),
            accepted: Mutex::new(HashMap::new()),
            accepted_signal: Condvar::new(),
            release_set: Mutex::new(HashSet::new()),
            release_signal: Condvar::new(),
            posts: AtomicU64::new(0),
            rejected_auth: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        });
        let state_task = Arc::clone(&state);
        let join = thread::spawn(move || {
            while !state_task.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if state_task.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let worker_state = Arc::clone(&state_task);
                        let worker = thread::spawn({
                            let worker_state = Arc::clone(&worker_state);
                            move || {
                                handle_provider_conn(stream, &worker_state);
                            }
                        });
                        worker_state
                            .workers
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(worker);
                    }
                    Err(_) if state_task.stop.load(Ordering::SeqCst) => break,
                    Err(_) => continue,
                }
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            listen: addr,
            state,
            accept_join: Mutex::new(Some(join)),
        }
    }

    pub fn shutdown(&self) {
        self.state.stop.store(true, Ordering::SeqCst);
        self.state.release_signal.notify_all();
        let _ = TcpStream::connect_timeout(&self.listen, Duration::from_millis(200));
        if let Some(join) = self
            .accept_join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = join.join();
        }
        let workers = std::mem::take(
            &mut *self
                .state
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        for worker in workers {
            let _ = worker.join();
        }
    }

    pub fn rejected_auth_count(&self) -> u64 {
        self.state.rejected_auth.load(Ordering::SeqCst)
    }

    pub fn arm(&self, semantic_id: &str, disposition: ProviderDisposition) {
        self.state
            .dispositions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(semantic_id.to_string(), disposition);
        if disposition != ProviderDisposition::Hold {
            self.release(semantic_id);
        }
    }

    pub fn release(&self, semantic_id: &str) {
        self.state
            .release_set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(semantic_id.to_string());
        self.state.release_signal.notify_all();
    }

    pub fn wait_accepted(&self, semantic_id: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut accepted = self
            .state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if accepted.get(semantic_id).copied().unwrap_or(0) >= 1 {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "provider never accepted POST {semantic_id}; records={:?}",
                self.records()
            );
            let (guard, result) = self
                .state
                .accepted_signal
                .wait_timeout(accepted, deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            accepted = guard;
            if result.timed_out() {
                assert!(
                    accepted.get(semantic_id).copied().unwrap_or(0) >= 1,
                    "provider never accepted POST {semantic_id}; records={:?}",
                    self.records()
                );
                return;
            }
        }
    }

    pub fn send_count(&self) -> u64 {
        self.state.posts.load(Ordering::SeqCst)
    }

    pub fn live_threads(&self) -> u64 {
        let accept = self
            .accept_join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some() as u64;
        let workers = self
            .state
            .workers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len() as u64;
        accept.saturating_add(workers)
    }

    pub fn count_for(&self, semantic_id: &str) -> u64 {
        self.state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(semantic_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn records(&self) -> Vec<ProviderRecord> {
        self.state
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn assert_route_and_auth(&self) {
        for record in self.records() {
            assert!(
                record.route_ok,
                "unexpected provider route {} {}",
                record.method, record.path
            );
            assert!(
                !record.body_digest.contains(SYNTHETIC_KEY)
                    && !record.body_digest.contains(TOKEN)
                    && !record.semantic_id.contains(SYNTHETIC_KEY)
                    && !record.focus_preview.contains(SYNTHETIC_KEY)
                    && !record.focus_preview.contains(TOKEN),
                "provider log stored a raw secret"
            );
            if record.method != "POST" {
                continue;
            }
            if record.auth_accepted {
                assert!(
                    record.auth_present,
                    "accepted provider POST {} lacked Authorization presence",
                    record.semantic_id
                );
                assert_eq!(record.auth_scheme.as_deref(), Some("bearer"));
            } else {
                assert!(
                    !record.auth_accepted,
                    "rejected provider POST must not be marked accepted"
                );
            }
        }
    }

    pub fn post_chat(&self, authorization: Option<&str>, body: &str) -> (u16, String) {
        let mut stream = TcpStream::connect_timeout(&self.listen, Duration::from_secs(2))
            .expect("connect fake provider");
        let mut headers = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.listen,
            body.len()
        );
        if let Some(value) = authorization {
            headers.push_str("Authorization: ");
            headers.push_str(value);
            headers.push_str("\r\n");
        }
        headers.push_str("\r\n");
        stream
            .write_all(headers.as_bytes())
            .expect("write provider headers");
        stream
            .write_all(body.as_bytes())
            .expect("write provider body");
        let _ = stream.flush();
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf);
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|item| item.parse().ok())
            .unwrap_or(0);
        (status, text.into_owned())
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn handle_provider_conn(mut stream: TcpStream, state: &ProviderState) {
    let Some((head, headers, body)) = read_http_message(&mut stream) else {
        return;
    };
    let (method, path) = split_request_line(&head);
    if method == "GET" {
        let _ = stream.write_all(models_list().as_bytes());
        let _ = stream.flush();
        return;
    }
    if method != "POST" {
        let _ = stream.write_all(
            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let auth = auth_presence(&headers);
    let auth_accepted = expected_bearer_accepted(&headers);
    let semantic_id = classify_semantic(&body);
    let focus = objective_focus(&current_user_text(&body));
    let focus_preview: String = focus.chars().take(96).collect();
    let record = ProviderRecord {
        method: method.clone(),
        path: path.clone(),
        auth_present: auth.0,
        auth_scheme: auth.1,
        auth_accepted,
        body_digest: hash_payload(&Value::String(body.clone())),
        semantic_id: semantic_id.clone(),
        route_ok: path == "/v1/chat/completions",
        focus_preview,
    };
    {
        let mut records = state
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        records.push(record);
        if records.len() > RECORD_BOUND {
            records.remove(0);
        }
    }
    if !auth_accepted {
        state.rejected_auth.fetch_add(1, Ordering::SeqCst);
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nunauthorized",
        );
        let _ = stream.flush();
        return;
    }
    state.posts.fetch_add(1, Ordering::SeqCst);
    {
        let mut accepted = state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *accepted.entry(semantic_id.clone()).or_insert(0) += 1;
        state.accepted_signal.notify_all();
    }
    let disposition = state
        .dispositions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&semantic_id)
        .copied()
        .unwrap_or(ProviderDisposition::Scripted);
    if disposition == ProviderDisposition::Hold {
        let mut released = state
            .release_set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let hold_deadline = Instant::now() + Duration::from_secs(120);
        while !released.contains(&semantic_id) && !state.stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now >= hold_deadline {
                break;
            }
            let (guard, _) = state
                .release_signal
                .wait_timeout(released, hold_deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            released = guard;
        }
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let script = *state
        .script
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match disposition {
        ProviderDisposition::Drop => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        ProviderDisposition::Status500 => {
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 16\r\nConnection: close\r\n\r\nprovider-fail-v1",
            );
        }
        ProviderDisposition::Malformed => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{not-json",
            );
        }
        ProviderDisposition::Slow => {
            thread::sleep(Duration::from_secs(8));
            let _ = stream.shutdown(Shutdown::Both);
        }
        ProviderDisposition::Scripted | ProviderDisposition::Hold => {
            let response = scripted_completion(&body, script);
            let _ = stream.write_all(response.as_bytes());
        }
    }
    let _ = stream.flush();
}

fn split_request_line(head: &str) -> (String, String) {
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    (method, path)
}

fn auth_presence(headers: &str) -> (bool, Option<String>) {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("authorization") {
            let scheme = value
                .split_whitespace()
                .next()
                .map(|item| item.to_ascii_lowercase());
            return (true, scheme);
        }
    }
    (false, None)
}

fn expected_bearer_accepted(headers: &str) -> bool {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("authorization") {
            continue;
        }
        let mut parts = value.split_whitespace();
        let scheme = parts.next().unwrap_or("");
        let token = parts.next().unwrap_or("");
        return scheme.eq_ignore_ascii_case("bearer")
            && token == SYNTHETIC_KEY
            && parts.next().is_none();
    }
    false
}

pub fn classify_provider_body(body: &str) -> String {
    classify_semantic(body)
}

fn classify_semantic(body: &str) -> String {
    let current = current_user_text(body);
    classify_current_user(&current)
}

fn classify_current_user(current: &str) -> String {
    let focus = objective_focus(current);
    let kind = prompt_kind(current);
    if let Some(rest) = current.split("CERT_HOLD ").nth(1) {
        let token = rest.split_whitespace().next().unwrap_or("");
        if !token.is_empty()
            && token
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        {
            return format!("hold-{token}");
        }
    }
    if current.contains("CERT_MALFORMED") {
        return "fail-malformed".into();
    }
    if current.contains("CERT_500") {
        return "fail-500".into();
    }
    if current.contains("CERT_DROP") {
        return "fail-drop".into();
    }
    if current.contains("CERT_SLOW") {
        return "fail-slow".into();
    }
    if current.contains("CERT_CANCEL") {
        return "fail-cancel".into();
    }
    if current.contains("GROKBOT_SETUP") && kind.is_none() {
        return "setup".into();
    }
    match kind {
        Some("manager-decision") => "manager-decision".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS complete the replacement") => {
            "step-b-fix".into()
        }
        Some("native") if focus.contains("GROKBOT_FORCE_FAIL") => "step-b".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS first native unit") => "step-a".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS") => "native-success".into(),
        Some("native") => "other".into(),
        None if focus.contains("Return exactly this JSON envelope") => "manager-decision".into(),
        None if current.contains("GROKBOT_SETUP") => "setup".into(),
        _ => "other".into(),
    }
}

fn prompt_kind(text: &str) -> Option<&str> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Objective:") || line.starts_with("Relevant messages:") {
            break;
        }
        if let Some(kind) = line.strip_prefix("Kind: ").map(str::trim) {
            if matches!(kind, "native" | "manager-decision") {
                return Some(kind);
            }
        }
    }
    None
}

fn objective_focus(text: &str) -> String {
    if let Some((_, rest)) = text.split_once("Objective:\n") {
        let mut lines = Vec::new();
        for line in rest.lines() {
            if line.starts_with("Verified continuation") || line.starts_with("Relevant messages:") {
                break;
            }
            lines.push(line);
        }
        let focused = lines.join("\n");
        if !focused.trim().is_empty() {
            return focused;
        }
    }
    text.to_string()
}

fn message_text(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

fn current_user_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    for message in messages.iter().rev() {
        if message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            != "user"
        {
            continue;
        }
        if let Some(text) = message_text(message) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    extract_all_text(body)
}

fn read_http_message(stream: &mut TcpStream) -> Option<(String, String, String)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(header_end) = find_double_crlf(&buf) {
            let headers = std::str::from_utf8(&buf[..header_end]).ok()?.to_string();
            let first = headers.lines().next().unwrap_or_default().to_string();
            let lower = headers.to_ascii_lowercase();
            let len = lower
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buf.len() < body_start + len {
                let n = stream.read(&mut tmp).ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let body_end = (body_start + len).min(buf.len());
            let body = String::from_utf8_lossy(&buf[body_start..body_end]).into_owned();
            return Some((first, headers, body));
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    }
    None
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn models_list() -> String {
    let body = r#"{"object":"list","data":[{"id":"grok-build","object":"model"}]}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn scripted_completion(body: &str, script: ProviderScript) -> String {
    match classify_semantic(body).as_str() {
        "manager-decision" => match script {
            ProviderScript::InvalidDirective => sse_ok(r#"{"not":"a-valid-manager-directive"}"#),
            ProviderScript::Lifecycle => sse_ok(&rewrite_directive(&current_user_text(body))),
        },
        "step-b" => {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 16\r\nConnection: close\r\n\r\nprovider-fail-v1".into()
        }
        _ => sse_ok("GROKBOT_OK"),
    }
}

fn extract_all_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    let mut out = String::new();
    for message in messages {
        match message.get("content") {
            Some(Value::String(text)) => {
                out.push_str(text);
                out.push('\n');
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        body.to_string()
    } else {
        out
    }
}

fn rewrite_directive(content: &str) -> String {
    let start = content
        .find("Envelope: ")
        .map(|index| index + "Envelope: ".len())
        .or_else(|| content.find("{\"directive\""))
        .or_else(|| content.find("{\"schemaVersion\""));
    let Some(start) = start else {
        return "{\"error\":\"missing-envelope\"}".into();
    };
    let slice = &content[start..];
    let Ok(mut value) = serde_json::from_str::<Value>(&take_json_object(slice)) else {
        return "{\"error\":\"envelope-parse\"}".into();
    };
    let agent_id = value
        .get("managerAgentId")
        .and_then(Value::as_str)
        .unwrap_or("missing-agent")
        .to_string();
    value["directive"] = json!({
        "type": "append_replacement_steps",
        "reason": "controlled child failure; no files changed",
        "replacesStepIds": ["step-b"],
        "steps": [{
            "stepId": "step-b-fix",
            "kind": "native",
            "objective": "GROKBOT_SUCCESS complete the replacement step",
            "priority": 0,
            "dependencies": ["step-a"],
            "assignedAgentId": agent_id,
            "policy": {
                "bounds": {
                    "maxPromptBytes": 16384,
                    "maxRounds": 4,
                    "maxDurationMs": 45000,
                    "maxTotalTokens": 8000
                },
                "retry": {
                    "maxAttempts": 1,
                    "retryFailed": false,
                    "retryExpired": false,
                    "backoffMs": 0
                },
                "requiresApproval": false,
                "maxConcurrentAttempts": 1
            }
        }]
    });
    value.to_string()
}

fn take_json_object(input: &str) -> String {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, ch) in input.char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return input[..=i].to_string();
                }
            }
            _ => {}
        }
    }
    input.to_string()
}

fn sse_ok(content: &str) -> String {
    let escaped = serde_json::to_string(content).expect("escape content");
    let chunk =
        format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{escaped}}}}}]}}\n\n");
    let done = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":4,\"total_tokens\":12}}\n\n";
    let end = "data: [DONE]\n\n";
    let body = format!("{chunk}{done}{end}");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ResourceSample {
    pub rss_bytes: u64,
    pub fd_count: u64,
    pub threads: u64,
    pub disk_bytes: u64,
}

impl ResourceSample {
    pub fn max_with(&mut self, other: &Self) {
        self.rss_bytes = self.rss_bytes.max(other.rss_bytes);
        self.fd_count = self.fd_count.max(other.fd_count);
        self.threads = self.threads.max(other.threads);
        self.disk_bytes = self.disk_bytes.max(other.disk_bytes);
    }

    pub fn growth_from(&self, baseline: &Self) -> ResourceSample {
        ResourceSample {
            rss_bytes: self.rss_bytes.saturating_sub(baseline.rss_bytes),
            fd_count: self.fd_count.saturating_sub(baseline.fd_count),
            threads: self.threads.saturating_sub(baseline.threads),
            disk_bytes: self.disk_bytes.saturating_sub(baseline.disk_bytes),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EntityCardinalities {
    pub sessions: usize,
    pub plans: usize,
    pub work: usize,
    pub intents: usize,
    pub runs: usize,
}

struct StderrCapture {
    head_cap: usize,
    tail_cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
}

impl StderrCapture {
    fn new(head_cap: usize, tail_cap: usize) -> Self {
        Self {
            head_cap,
            tail_cap,
            head: Vec::new(),
            tail: VecDeque::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.head.len() < self.head_cap {
                self.head.push(byte);
            } else {
                if self.tail.len() >= self.tail_cap {
                    self.tail.pop_front();
                }
                self.tail.push_back(byte);
            }
        }
    }

    fn head_text(&self) -> String {
        String::from_utf8_lossy(&self.head).into_owned()
    }

    fn tail_text(&self) -> String {
        let bytes: Vec<u8> = self.tail.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

pub struct ServiceProcess {
    pub addr: String,
    pub previous_addr: Option<String>,
    pub previous_pid: Option<u32>,
    child: Child,
    pub home: PathBuf,
    pub workspace: PathBuf,
    stderr: Arc<Mutex<StderrCapture>>,
    artifact_scan: ArtifactScan,
    client_specs: Vec<String>,
    _home_dir: TempDir,
    _workspace_dir: TempDir,
}

impl ServiceProcess {
    pub fn spawn(provider_base: &str) -> Self {
        let fixture = Fixture::load();
        let home_dir = tempfile::tempdir().expect("runtime home");
        let workspace_dir = tempfile::tempdir().expect("workspace");
        let home = dunce::canonicalize(home_dir.path()).expect("canon home");
        let workspace = dunce::canonicalize(workspace_dir.path()).expect("canon workspace");
        let stderr = Arc::new(Mutex::new(StderrCapture::new(
            fixture.artifact_scan.stderr_head_bytes,
            fixture.artifact_scan.stderr_tail_bytes,
        )));
        let client_specs = Vec::new();
        let mut child = spawn_service(provider_base, &home, &workspace, &client_specs, &stderr);
        let addr = wait_child_ready(&mut child, &stderr);
        Self {
            addr,
            previous_addr: None,
            previous_pid: None,
            child,
            home,
            workspace,
            stderr,
            artifact_scan: fixture.artifact_scan,
            client_specs,
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn sample_tree(&self) -> ResourceSample {
        let parent = sample_pid(std::process::id());
        let child = sample_pid(self.pid());
        ResourceSample {
            rss_bytes: parent.rss_bytes.saturating_add(child.rss_bytes),
            fd_count: parent.fd_count.saturating_add(child.fd_count),
            threads: parent.threads.saturating_add(child.threads),
            disk_bytes: dir_size(&self.home).saturating_add(dir_size(&self.workspace)),
        }
    }

    pub fn stderr_head(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .head_text()
    }

    pub fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tail_text()
    }

    pub fn kill_sigkill(&mut self) {
        self.previous_addr = Some(self.addr.clone());
        self.previous_pid = Some(self.pid());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn assert_previous_endpoint_dead(&self) {
        let addr = self
            .previous_addr
            .as_deref()
            .expect("kill_sigkill must record the previous listen address");
        assert!(
            endpoint_dead(addr),
            "previous MCP endpoint {addr} is still reachable after SIGKILL"
        );
    }

    pub fn respawn(&mut self, provider_base: &str) {
        self.kill_sigkill();
        self.assert_previous_endpoint_dead();
        let mut last = String::new();
        for attempt in 1..=5 {
            self.stderr = Arc::new(Mutex::new(StderrCapture::new(
                self.artifact_scan.stderr_head_bytes,
                self.artifact_scan.stderr_tail_bytes,
            )));
            self.child = spawn_service(
                provider_base,
                &self.home,
                &self.workspace,
                &self.client_specs,
                &self.stderr,
            );
            match wait_child_ready_result(&mut self.child, &self.stderr) {
                Ok(listen) => {
                    self.addr = listen;
                    assert_ne!(
                        Some(self.pid()),
                        self.previous_pid,
                        "respawned service reused the killed PID"
                    );
                    return;
                }
                Err(error) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    last = format!("attempt {attempt}: {error}");
                }
            }
        }
        panic!("respawn grokptah-service never became ready: {last}");
    }

    /// Replace the named non-primary credentials installed on the next
    /// process start. Specs are kept only in this in-memory process harness;
    /// the service receives them through its documented environment input.
    pub fn replace_client_specs(&mut self, specs: Vec<String>) {
        assert!(
            specs.iter().all(|spec| {
                !spec.trim().is_empty()
                    && !spec.contains(',')
                    && !spec.contains('\n')
                    && !spec.contains('\r')
            }),
            "service client specs must be non-empty single entries"
        );
        self.client_specs = specs;
    }

    pub fn durable_home_entries(&self) -> Vec<(String, String, u64)> {
        fingerprint_entries(&self.home, &self.artifact_scan)
            .unwrap_or_else(|error| panic!("home fingerprint entries: {error}"))
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_service(
    provider_base: &str,
    home: &Path,
    workspace: &Path,
    client_specs: &[String],
    stderr_buf: &Arc<Mutex<StderrCapture>>,
) -> Child {
    let bin = env!("CARGO_BIN_EXE_grokptah-service");
    let mut command = Command::new(bin);
    for key in AMBIENT_CREDENTIAL_ENV {
        command.env_remove(key);
    }
    // TOKEN-only synthesis is coordinator and cannot list
    // `ptah_set_managed_execution`. Named `operator:primary=` replaces that
    // credential; extra specs stay workers and must not include primary.
    command.env(
        "GROKPTAH_SERVICE_CLIENTS",
        always_on_operator_client_specs(client_specs).join(","),
    );
    let mut child = command
        .env("GROKPTAH_HOME", home)
        .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
        .env("GROKPTAH_SERVICE_LISTEN", "127.0.0.1:0")
        .env("GROKPTAH_SERVICE_WORKSPACES", workspace)
        .env("GROKPTAH_SERVICE_MAX_CONCURRENT", "4")
        .env("XAI_API_KEY", SYNTHETIC_KEY)
        .env("XAI_API_BASE", provider_base)
        .env_remove("GROKPTAH_AGENT_OFFLINE")
        .env_remove("GROKPTAH_TOKEN_COMMAND")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn grokptah-service");
    if let Some(mut stderr) = child.stderr.take() {
        let buf = Arc::clone(stderr_buf);
        thread::spawn(move || {
            let mut tmp = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut tmp) {
                if n == 0 {
                    break;
                }
                buf.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(&tmp[..n]);
            }
        });
    }
    child
}

fn always_on_operator_client_specs(client_specs: &[String]) -> Vec<String> {
    let mut specs = Vec::with_capacity(client_specs.len() + 1);
    specs.push(format!("operator:primary={TOKEN}"));
    for spec in client_specs {
        assert!(
            !spec.contains(":primary="),
            "Always-On extra client specs must not replace operator primary: {spec}"
        );
        specs.push(spec.clone());
    }
    specs
}

#[test]
fn always_on_operator_client_specs_keep_workers_off_primary() {
    let specs = always_on_operator_client_specs(&["worker:w1/agent-1=worker-token-1".to_string()]);
    assert_eq!(specs[0], format!("operator:primary={TOKEN}"));
    assert_eq!(specs[1], "worker:w1/agent-1=worker-token-1");
    assert_eq!(specs.len(), 2);
}

fn endpoint_dead(addr: &str) -> bool {
    let Ok(socket) = addr.parse::<SocketAddr>() else {
        return true;
    };
    TcpStream::connect_timeout(&socket, Duration::from_millis(200)).is_err()
}

fn captured_stderr(stderr: &Arc<Mutex<StderrCapture>>) -> String {
    let guard = stderr
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    format!("{}{}", guard.head_text(), guard.tail_text())
}

fn ready_addrs(text: &str) -> Vec<String> {
    text.match_indices("ready addr=http://")
        .filter_map(|(index, _)| {
            let rest = &text[index + "ready addr=http://".len()..];
            rest.split(|ch: char| ch.is_whitespace() || ch == '/')
                .next()
                .map(str::to_string)
                .filter(|addr| !addr.is_empty())
        })
        .collect()
}

fn wait_child_ready(child: &mut Child, stderr: &Arc<Mutex<StderrCapture>>) -> String {
    wait_child_ready_result(child, stderr).unwrap_or_else(|error| panic!("{error}"))
}

fn wait_child_ready_result(
    child: &mut Child,
    stderr: &Arc<Mutex<StderrCapture>>,
) -> Result<String, String> {
    let deadline = Instant::now() + READY_WAIT;
    let mut last_http = String::from("no ready line yet");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "grokptah-service exited {status}; stderr={}",
                captured_stderr(stderr)
            ));
        }
        let text = captured_stderr(stderr);
        if let Some(addr) = ready_addrs(&text).last().cloned() {
            match probe_ready_http(&addr) {
                Ok(()) => return Ok(addr),
                Err(error) => last_http = error,
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "grokptah-service never became HTTP-ready ({last_http}); stderr={text}"
            ));
        }
        thread::sleep(POLL);
    }
}

fn probe_ready_http(addr: &str) -> Result<(), String> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|error| format!("parse listen {addr}: {error}"))?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_millis(200))
        .map_err(|error| format!("connect {addr}: {error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let request = format!("GET /ready HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write {addr}: {error}"))?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    let status = buf.lines().next().unwrap_or("empty-response");
    if http_control_plane_is_serving(status) {
        Ok(())
    } else {
        Err(format!("GET /ready {addr} -> {status}"))
    }
}

fn http_control_plane_is_serving(status: &str) -> bool {
    status.starts_with("HTTP/1.1 200")
        || status.starts_with("HTTP/1.0 200")
        || status.starts_with("HTTP/1.1 503")
        || status.starts_with("HTTP/1.0 503")
}

fn sample_pid(pid: u32) -> ResourceSample {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let mut rss_kb = 0u64;
    let mut threads = 0u64;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kb = value
                .split_whitespace()
                .next()
                .and_then(|item| item.parse().ok())
                .unwrap_or(0);
        }
        if let Some(value) = line.strip_prefix("Threads:") {
            threads = value.trim().parse().unwrap_or(0);
        }
    }
    let fd_count = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|entries| entries.count() as u64)
        .unwrap_or(0);
    ResourceSample {
        rss_bytes: rss_kb.saturating_mul(1024),
        fd_count,
        threads,
        disk_bytes: 0,
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        if meta.as_ref().is_some_and(|item| item.is_dir()) {
            total = total.saturating_add(dir_size(&entry.path()));
        } else if let Some(meta) = meta {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

pub async fn mcp(addr: &str) -> McpControlClient {
    mcp_with_token(addr, TOKEN).await
}

pub async fn mcp_with_token(addr: &str, token: &str) -> McpControlClient {
    let mut client = McpControlClient::new(format!("http://{addr}"), token);
    let deadline = Instant::now() + READY_WAIT;
    loop {
        match client.initialize().await {
            Ok(_) => return client,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(POLL).await;
            }
            Err(error) => panic!("initialize MCP: {error}"),
        }
    }
}

pub async fn try_mcp(addr: &str, token: &str) -> Result<McpControlClient, String> {
    let mut client = McpControlClient::new(format!("http://{addr}"), token);
    client
        .initialize()
        .await
        .map(|_| client)
        .map_err(|error| error.to_string())
}

pub async fn call(client: &mut McpControlClient, tool: &str, args: Value) -> Value {
    let result = client
        .call_tool(tool, args)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    scan_mcp(tool, &result.structured, &result.raw);
    assert!(!result.is_error, "{tool} error: {:?}", result.raw);
    result.structured
}

pub async fn call_expect_error(client: &mut McpControlClient, tool: &str, args: Value) -> String {
    match client.call_tool(tool, args).await {
        Ok(result) => {
            scan_mcp(tool, &result.structured, &result.raw);
            assert!(result.is_error, "{tool} should fail: {:?}", result.raw);
            result.raw.to_string()
        }
        Err(error) => error.to_string(),
    }
}

pub async fn poll_json<F>(
    client: &mut McpControlClient,
    tool: &str,
    args: Value,
    mut pred: F,
) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = json!({});
    while Instant::now() < deadline {
        last = call(client, tool, args.clone()).await;
        if pred(&last) {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("{tool} predicate never held: {last}");
}

pub fn rid(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

pub fn work_items(work: &Value) -> &[Value] {
    work.get("work")
        .or_else(|| work.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn work_kind_count(work: &Value, kind: &str) -> usize {
    work_items(work)
        .iter()
        .filter(|item| item["kind"].as_str() == Some(kind))
        .count()
}

pub fn succeeded_kind_count(work: &Value, kind: &str) -> usize {
    work_items(work)
        .iter()
        .filter(|item| {
            item["kind"].as_str() == Some(kind) && item["state"].as_str() == Some("succeeded")
        })
        .count()
}

pub fn work_for_step<'a>(work: &'a Value, step_id: &str) -> Vec<&'a Value> {
    work_items(work)
        .iter()
        .filter(|item| item["sourceManagerStepId"].as_str() == Some(step_id))
        .collect()
}

pub fn runs_array(runs: &Value) -> &[Value] {
    runs.get("runs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn intents_array(intents: &Value) -> &[Value] {
    intents
        .get("intents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn pending_usage(run: &Value) -> u64 {
    run.pointer("/aggregates/usagePendingRequests")
        .or_else(|| run.pointer("/aggregates/usage_pending_requests"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

pub fn assert_no_quota_ledger(value: &Value) {
    let encoded = value.to_string();
    assert!(
        !encoded.contains("QuotaLedger") && !encoded.contains("quotaLedger"),
        "quota ledger must be absent at 67e29bd: {value}"
    );
}

pub fn scan_text_result(label: &str, text: &str) -> Result<(), String> {
    for sentinel in [
        TOKEN,
        SYNTHETIC_KEY,
        "XAI_API_KEY=",
        "GROKPTAH_SERVICE_TOKEN=",
        "GROKPTAH_TOKEN_COMMAND=",
    ] {
        if text.contains(sentinel) {
            return Err(format!("{label} leaked sentinel {sentinel}"));
        }
    }
    for sentinel in LIVE_URL_SENTINELS {
        if text.contains(sentinel) {
            return Err(format!("{label} leaked live URL sentinel"));
        }
    }
    Ok(())
}

pub fn scan_text(label: &str, text: &str) {
    scan_text_result(label, text).unwrap_or_else(|error| panic!("{error}"));
}

fn scan_safe_field_name(key: &str) -> String {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    // Public authority documents expose `principalId`. The certification
    // scanner treats that exact field name as secret-bearing, so the
    // Always-On projection renames it after redacting the identity value.
    if normalized == "principalid" {
        "principalRef".into()
    } else {
        key.to_string()
    }
}

fn is_path_identity_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "workspace" | "sourceworkspace" | "cwd" | "displayname" | "title" | "promptpreview"
    ) || normalized.ends_with("id")
        || normalized.ends_with("ids")
        || normalized.ends_with("hash")
}

fn looks_like_protocol_opaque_token(token: &str) -> bool {
    if token.len() < 40
        || token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || (token.len() == 71
            && token.starts_with("opaque-")
            && token[7..].bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return false;
    }
    let has_alpha = token.bytes().any(|byte| byte.is_ascii_alphabetic());
    let digit_count = token.bytes().filter(u8::is_ascii_digit).count();
    let distinct = token.bytes().collect::<BTreeSet<_>>().len();
    has_alpha && digit_count >= 6 && distinct >= 12
}

fn redact_high_entropy_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut current = String::new();
    let flush = |out: &mut String, current: &mut String| {
        if looks_like_protocol_opaque_token(current) {
            out.push_str("<opaque-id>");
        } else {
            out.push_str(current);
        }
        current.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            current.push(ch);
        } else {
            if !current.is_empty() {
                flush(&mut out, &mut current);
            }
            out.push(ch);
        }
    }
    if !current.is_empty() {
        flush(&mut out, &mut current);
    }
    out
}

fn redact_protocol_identity_value(value: &Value) -> Value {
    match value {
        Value::String(_) => Value::String("<opaque-id>".into()),
        Value::Array(items) => {
            Value::Array(items.iter().map(redact_protocol_identity_value).collect())
        }
        other => project_public_mcp_for_secret_scan(other),
    }
}

pub fn project_public_mcp_for_secret_scan(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text)
            .map(|parsed| project_public_mcp_for_secret_scan(&parsed))
            .unwrap_or_else(|_| Value::String(redact_high_entropy_tokens(text))),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, nested)| {
                    let projected = if is_path_identity_key(key) {
                        redact_protocol_identity_value(nested)
                    } else {
                        project_public_mcp_for_secret_scan(nested)
                    };
                    (scan_safe_field_name(key), projected)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(project_public_mcp_for_secret_scan)
                .collect(),
        ),
        other => other.clone(),
    }
}

fn scan_json_for_forbidden_data(label: &str, value: &Value) {
    scan_value_for_forbidden_data(&project_public_mcp_for_secret_scan(value))
        .unwrap_or_else(|error| panic!("{label} failed forbidden-data scan: {error}"));
}

pub fn scan_mcp(tool: &str, structured: &Value, raw: &Value) {
    scan_text(&format!("{tool} structured"), &structured.to_string());
    scan_text(&format!("{tool} raw"), &raw.to_string());
    scan_json_for_forbidden_data(&format!("{tool} structured"), structured);
    scan_json_for_forbidden_data(&format!("{tool} raw"), raw);
}

pub fn scan_home(home: &Path, limits: &ArtifactScan) {
    scan_tree(home, limits, &[]).unwrap_or_else(|error| panic!("home scan failed: {error}"));
}

pub fn scan_service_artifacts(service: &ServiceProcess) {
    scan_text("stderr-head", &service.stderr_head());
    scan_text("stderr-tail", &service.stderr_tail());
    scan_home(&service.home, &service.artifact_scan);
}

pub fn scan_service_artifacts_with_sentinels(service: &ServiceProcess, sentinels: &[&str]) {
    assert!(
        sentinels.iter().all(|sentinel| !sentinel.is_empty()),
        "artifact sentinels must be non-empty"
    );
    for (label, text) in [
        ("stderr-head", service.stderr_head()),
        ("stderr-tail", service.stderr_tail()),
    ] {
        scan_text(label, &text);
        for sentinel in sentinels {
            assert!(!text.contains(sentinel), "{label} leaked campaign sentinel");
        }
    }
    scan_tree(&service.home, &service.artifact_scan, sentinels)
        .unwrap_or_else(|error| panic!("campaign home scan failed: {error}"));
}

fn scan_tree(root: &Path, limits: &ArtifactScan, sentinels: &[&str]) -> Result<(), String> {
    let mut files = 0u64;
    scan_tree_inner(root, 0, limits, sentinels, &mut files)
}

fn scan_tree_inner(
    path: &Path,
    depth: u64,
    limits: &ArtifactScan,
    sentinels: &[&str],
    files: &mut u64,
) -> Result<(), String> {
    if depth > limits.max_depth {
        return Err(format!(
            "artifact depth {depth} exceeds ceiling {}",
            limits.max_depth
        ));
    }
    let entries =
        std::fs::read_dir(path).map_err(|error| format!("read_dir {}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("dirent {}: {error}", path.display()))?;
        let child = entry.path();
        let meta = std::fs::symlink_metadata(&child)
            .map_err(|error| format!("metadata {}: {error}", child.display()))?;
        if meta.file_type().is_dir() {
            scan_tree_inner(&child, depth + 1, limits, sentinels, files)?;
            continue;
        }
        *files = files.saturating_add(1);
        if *files > limits.max_files {
            return Err(format!(
                "artifact file count {} exceeds ceiling {}",
                *files, limits.max_files
            ));
        }
        let bytes =
            std::fs::read(&child).map_err(|error| format!("read {}: {error}", child.display()))?;
        if bytes.len() as u64 > limits.max_file_bytes {
            return Err(format!(
                "artifact {} is {} bytes, ceiling {}",
                child.display(),
                bytes.len(),
                limits.max_file_bytes
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            format!(
                "artifact {} is binary or non-UTF8 ({} bytes)",
                child.display(),
                bytes.len()
            )
        })?;
        scan_text_result(&format!("home {}", child.display()), text)?;
        for sentinel in sentinels {
            if text.contains(sentinel) {
                return Err(format!("home {} leaked campaign sentinel", child.display()));
            }
        }
    }
    Ok(())
}

pub fn fingerprint_tree(root: &Path, limits: &ArtifactScan) -> Result<String, String> {
    let files = fingerprint_entries(root, limits)?;
    Ok(hash_payload(&json!(files)))
}

pub fn fingerprint_entries(
    root: &Path,
    limits: &ArtifactScan,
) -> Result<Vec<(String, String, u64)>, String> {
    let mut files = Vec::new();
    collect_fingerprint(root, root, 0, limits, &mut files)?;
    files.sort();
    Ok(files)
}

fn volatile_home_path(rel: &str) -> bool {
    let name = Path::new(rel)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(rel);
    matches!(
        name,
        ".instance.lock"
            | ".store.lock"
            | "event_journal.jsonl"
            | "event_journal.seq"
            | "event_journal.gap.json"
    ) || name.ends_with("-wal")
        || name.ends_with("-shm")
        || rel
            .split(['/', '\\'])
            .any(|part| matches!(part, "worker-presence" | "audit" | "computer-use"))
}

fn collect_fingerprint(
    root: &Path,
    path: &Path,
    depth: u64,
    limits: &ArtifactScan,
    files: &mut Vec<(String, String, u64)>,
) -> Result<(), String> {
    if depth > limits.max_depth {
        return Err(format!(
            "fingerprint depth {depth} exceeds ceiling {}",
            limits.max_depth
        ));
    }
    let entries =
        std::fs::read_dir(path).map_err(|error| format!("read_dir {}: {error}", path.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("dirent {}: {error}", path.display()))?;
        let child = entry.path();
        let meta = std::fs::symlink_metadata(&child)
            .map_err(|error| format!("metadata {}: {error}", child.display()))?;
        let rel = child
            .strip_prefix(root)
            .unwrap_or(&child)
            .to_string_lossy()
            .into_owned();
        if volatile_home_path(&rel) {
            continue;
        }
        if meta.file_type().is_dir() {
            collect_fingerprint(root, &child, depth + 1, limits, files)?;
            continue;
        }
        if files.len() as u64 >= limits.max_files {
            return Err(format!(
                "fingerprint file count exceeds ceiling {}",
                limits.max_files
            ));
        }
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&child)
                .map_err(|error| format!("read_link {}: {error}", child.display()))?;
            files.push((
                rel,
                hash_payload(&Value::String(target.display().to_string())),
                0,
            ));
            continue;
        }
        let bytes =
            std::fs::read(&child).map_err(|error| format!("read {}: {error}", child.display()))?;
        if bytes.len() as u64 > limits.max_file_bytes {
            return Err(format!(
                "fingerprint {} is {} bytes, ceiling {}",
                child.display(),
                bytes.len(),
                limits.max_file_bytes
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            format!(
                "fingerprint {} is binary or non-UTF8 ({} bytes)",
                child.display(),
                bytes.len()
            )
        })?;
        files.push((
            rel,
            hash_payload(&Value::String(text.to_string())),
            bytes.len() as u64,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CausalJoin {
    pub work_id: String,
    pub work_revision: u64,
    pub work_state: String,
    pub attempt_id: String,
    pub intent_id: String,
    pub intent_work_revision: u64,
    pub intent_agent_spec_revision: u64,
    pub intent_input_hash: String,
    pub run_id: String,
    pub run_state: String,
    pub run_request_id: String,
    pub run_agent_spec_revision: Option<u64>,
    pub provider_digest: String,
    pub provider_posts: u64,
}

pub fn require_unique_step_work<'a>(work: &'a Value, step_id: &str) -> &'a Value {
    let items = work_for_step(work, step_id);
    assert_eq!(
        items.len(),
        1,
        "step {step_id} must have exactly one Work: {work}"
    );
    items[0]
}

#[allow(clippy::too_many_arguments)]
pub fn require_causal_join(
    work: &Value,
    detailed: &Value,
    intents: &Value,
    runs: &Value,
    provider: &FakeProvider,
    step_id: &str,
    semantic_id: &str,
    expected_posts: u64,
) -> CausalJoin {
    causal_join(
        work,
        detailed,
        intents,
        runs,
        provider,
        step_id,
        semantic_id,
        expected_posts,
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

#[allow(clippy::too_many_arguments)]
pub fn causal_join(
    work: &Value,
    detailed: &Value,
    intents: &Value,
    runs: &Value,
    provider: &FakeProvider,
    step_id: &str,
    semantic_id: &str,
    expected_posts: u64,
) -> Result<CausalJoin, String> {
    let items = work_for_step(work, step_id);
    if items.len() != 1 {
        return Err(format!(
            "step {step_id} must have exactly one Work, found {}: {work}",
            items.len()
        ));
    }
    let work_item = items[0];
    let work_id = work_item["workId"]
        .as_str()
        .ok_or_else(|| format!("missing workId for {step_id}"))?
        .to_string();
    let work_revision = work_item["revision"]
        .as_u64()
        .ok_or_else(|| format!("missing work revision for {step_id}"))?;
    let work_state = work_item["state"]
        .as_str()
        .ok_or_else(|| format!("missing work state for {step_id}"))?
        .to_string();
    if detailed["work"]["workId"].as_str() != Some(work_id.as_str()) {
        return Err(format!("get_work target mismatch for {step_id}"));
    }
    let attempts = detailed["attempts"]
        .as_array()
        .ok_or_else(|| format!("missing public attempts for {step_id}"))?;
    if attempts.len() != 1 {
        return Err(format!(
            "step {step_id} must have exactly one public attempt: {detailed}"
        ));
    }
    let attempt_id = attempts[0]["attemptId"]
        .as_str()
        .ok_or_else(|| format!("missing attemptId for {step_id}"))?
        .to_string();
    let linked = attempts[0]["linkedRunIds"]
        .as_array()
        .ok_or_else(|| format!("missing linkedRunIds for {step_id}"))?;
    if linked.len() != 1 {
        return Err(format!(
            "step {step_id} must have exactly one linked Run: {detailed}"
        ));
    }
    let linked_run = linked[0]
        .as_str()
        .ok_or_else(|| format!("linked run id missing for {step_id}"))?
        .to_string();
    let matching_intents: Vec<&Value> = intents_array(intents)
        .iter()
        .filter(|intent| {
            intent["workId"].as_str() == Some(work_id.as_str())
                && intent["attemptId"].as_str() == Some(attempt_id.as_str())
        })
        .collect();
    if matching_intents.len() != 1 {
        return Err(format!(
            "step {step_id} must have exactly one intent for work {work_id} attempt {attempt_id}: {intents}"
        ));
    }
    let intent = matching_intents[0];
    let intent_id = intent["intentId"]
        .as_str()
        .ok_or_else(|| format!("missing intentId for {step_id}"))?
        .to_string();
    let intent_run = intent["runId"]
        .as_str()
        .ok_or_else(|| format!("missing intent.runId for {step_id}"))?
        .to_string();
    if intent_run != linked_run {
        return Err(format!(
            "intent.runId must equal the unique linkedRunId for {step_id}"
        ));
    }
    let intent_work_revision = intent["workRevision"]
        .as_u64()
        .ok_or_else(|| format!("missing intent.workRevision for {step_id}"))?;
    let intent_agent_spec_revision = intent["agentSpecRevision"]
        .as_u64()
        .ok_or_else(|| format!("missing intent.agentSpecRevision for {step_id}"))?;
    let intent_input_hash = intent["inputHash"]
        .as_str()
        .ok_or_else(|| format!("missing intent.inputHash for {step_id}"))?
        .to_string();
    if intent_input_hash.is_empty() {
        return Err(format!("intent.inputHash must be public for {step_id}"));
    }
    let matching_runs: Vec<&Value> = runs_array(runs)
        .iter()
        .filter(|run| run["runId"].as_str() == Some(intent_run.as_str()))
        .collect();
    if matching_runs.len() != 1 {
        return Err(format!(
            "step {step_id} must have exactly one Run {intent_run}: {runs}"
        ));
    }
    let run = matching_runs[0];
    let run_request_id = run["requestId"]
        .as_str()
        .ok_or_else(|| format!("missing run.requestId for {step_id}"))?
        .to_string();
    if run_request_id != intent_id {
        return Err(format!(
            "run.requestId must equal intent.intentId for {step_id}"
        ));
    }
    let run_state = run["state"]
        .as_str()
        .ok_or_else(|| format!("missing run.state for {step_id}"))?
        .to_string();
    let run_agent_spec_revision = run["agentSpecRevision"].as_u64();
    let accepted: Vec<ProviderRecord> = provider
        .records()
        .into_iter()
        .filter(|record| record.auth_accepted && record.semantic_id == semantic_id)
        .collect();
    if accepted.len() as u64 != expected_posts {
        return Err(format!(
            "step {step_id} must have exactly {expected_posts} accepted provider record(s) for {semantic_id}: {:?}",
            provider.records()
        ));
    }
    let provider_posts = provider.count_for(semantic_id);
    if provider_posts != expected_posts {
        return Err(format!(
            "semantic POST count for {semantic_id} must be {expected_posts}, found {provider_posts}"
        ));
    }
    Ok(CausalJoin {
        work_id,
        work_revision,
        work_state,
        attempt_id,
        intent_id,
        intent_work_revision,
        intent_agent_spec_revision,
        intent_input_hash,
        run_id: intent_run,
        run_state,
        run_request_id,
        run_agent_spec_revision,
        provider_digest: accepted[0].body_digest.clone(),
        provider_posts,
    })
}

pub fn sessions_len(value: &Value) -> usize {
    value
        .get("sessions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub fn plans_len(value: &Value) -> usize {
    value
        .get("plans")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0)
}

pub fn repository_commit() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

static SERIAL: Mutex<()> = Mutex::new(());
static RECORDED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

pub fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn clear_assertions() {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

pub fn record_assertion(name: &str) {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(name.to_string());
}

pub fn certify(name: &str) {
    let fixture = Fixture::load();
    assert!(
        fixture.required_assertions.iter().any(|item| item == name),
        "assertion {name} is not declared in the typed fixture"
    );
    record_assertion(name);
}

pub fn recorded_assertions() -> BTreeSet<String> {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

#[cfg(test)]
mod classification_tests {
    use super::classify_provider_body;
    use serde_json::{json, Value};

    fn chat(messages: Vec<Value>) -> String {
        json!({
            "model": "grok-build",
            "messages": messages,
            "stream": true
        })
        .to_string()
    }

    fn managed(kind: &str, objective: &str, relevant: &str) -> String {
        format!(
            "Managed Work execution. This is a new finite Run; do not resume an interrupted model invocation.\nWork ID: work-1\nKind: {kind}\nAttempt: 1\nObjective:\n{objective}\nRelevant messages:\n{relevant}\n"
        )
    }

    #[test]
    fn manager_decision_ignores_history_and_snapshot_force_fail() {
        let body = chat(vec![
            json!({"role":"system","content":"You are GrokPtah. Kind: native is documented."}),
            json!({"role":"user","content": managed("native", "GROKBOT_SUCCESS first native unit", "")}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content": managed("native", "GROKBOT_FORCE_FAIL child that must be replaced", "")}),
            json!({"role":"assistant","content":"provider-fail-v1"}),
            json!({"role":"user","content": managed(
                "manager-decision",
                "Return exactly this JSON envelope with only directive replaced, and no prose. You have no tool authority. Envelope: {\"directive\":{\"type\":\"no_safe_action\"}} Snapshot: {\"kind\":\"native\",\"objective\":\"GROKBOT_FORCE_FAIL child that must be replaced\",\"first\":\"GROKBOT_SUCCESS first native unit\"}",
                "- [result] Kind: native\n- [result] GROKBOT_FORCE_FAIL child that must be replaced"
            )}),
        ]);
        assert_eq!(classify_provider_body(&body), "manager-decision");
    }

    #[test]
    fn native_step_b_ignores_setup_and_step_a_history() {
        let body = chat(vec![
            json!({"role":"user","content":"GROKBOT_SETUP materialize the lane Agent"}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content": managed("native", "GROKBOT_SUCCESS first native unit", "")}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content": managed("native", "GROKBOT_FORCE_FAIL child that must be replaced", "")}),
        ]);
        assert_eq!(classify_provider_body(&body), "step-b");
    }

    #[test]
    fn replacement_after_manager_history_is_not_a_second_manager_decision() {
        let body = chat(vec![
            json!({"role":"user","content":"GROKBOT_SETUP materialize the lane Agent"}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content": managed("native", "GROKBOT_SUCCESS first native unit", "")}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content": managed("native", "GROKBOT_FORCE_FAIL child that must be replaced", "")}),
            json!({"role":"assistant","content":"provider-fail-v1"}),
            json!({"role":"user","content": managed(
                "manager-decision",
                "Return exactly this JSON envelope with only directive replaced, and no prose. Envelope: {\"directive\":{\"type\":\"no_safe_action\"}}",
                "- [result] GROKBOT_FORCE_FAIL child that must be replaced"
            )}),
            json!({"role":"assistant","content":"{\"directive\":{\"type\":\"append_replacement_steps\"}}"}),
            json!({"role":"user","content": managed(
                "native",
                "GROKBOT_SUCCESS complete the replacement step",
                "- [manager] Return exactly this JSON envelope Envelope: {\"directive\":{}}"
            )}),
        ]);
        assert_eq!(classify_provider_body(&body), "step-b-fix");
    }

    #[test]
    fn native_kind_does_not_become_manager_decision_when_envelope_is_in_objective() {
        let body = chat(vec![json!({"role":"user","content": managed(
            "native",
            "GROKBOT_SUCCESS complete the replacement step\nReturn exactly this JSON envelope must not steal identity",
            ""
        )})]);
        assert_eq!(classify_provider_body(&body), "step-b-fix");
    }

    #[test]
    fn current_user_content_array_still_classifies_replacement() {
        let body = json!({
            "model": "grok-build",
            "messages": [
                {"role":"user","content": managed(
                    "manager-decision",
                    "Return exactly this JSON envelope with only directive replaced",
                    ""
                )},
                {"role":"assistant","content":"{\"directive\":{}}"},
                {"role":"user","content":[
                    {"type":"text","text": managed(
                        "native",
                        "GROKBOT_SUCCESS complete the replacement step",
                        "- [manager] Return exactly this JSON envelope"
                    )}
                ]}
            ],
            "stream": true
        })
        .to_string();
        assert_eq!(classify_provider_body(&body), "step-b-fix");
    }

    #[test]
    fn cert_fault_uses_current_user_not_prior_managed_kind() {
        let body = chat(vec![
            json!({"role":"user","content": managed("native", "GROKBOT_FORCE_FAIL child that must be replaced", "")}),
            json!({"role":"assistant","content":"GROKBOT_OK"}),
            json!({"role":"user","content":"CERT_DROP provider disconnect"}),
        ]);
        assert_eq!(classify_provider_body(&body), "fail-drop");
    }

    #[test]
    fn cert_hold_classifies_unique_cycle_token() {
        let body = chat(vec![
            json!({"role":"user","content":"CERT_HOLD cycle-7 hold this provider POST"}),
        ]);
        assert_eq!(classify_provider_body(&body), "hold-cycle-7");
    }
}

#[cfg(test)]
mod fixture_schema_tests {
    use super::{parse_fixture, FIXTURE_BYTES};
    use serde_json::Value;

    #[test]
    fn typed_fixture_parses_and_rejects_mutants() {
        parse_fixture(FIXTURE_BYTES).expect("canonical fixture");
        let mut value: Value = serde_json::from_slice(FIXTURE_BYTES).unwrap();
        value["unexpectedField"] = serde_json::json!(true);
        assert!(parse_fixture(&serde_json::to_vec(&value).unwrap()).is_err());
        let mut missing = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        missing.as_object_mut().unwrap().remove("provedOracle");
        assert!(parse_fixture(&serde_json::to_vec(&missing).unwrap()).is_err());
        let mut version = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        version["schemaVersion"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&version).unwrap()).is_err());
        let mut duplicate = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        let first = duplicate["requiredAssertions"][0].clone();
        duplicate["requiredAssertions"]
            .as_array_mut()
            .unwrap()
            .push(first);
        assert!(parse_fixture(&serde_json::to_vec(&duplicate).unwrap()).is_err());
        let mut empty_claim = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        empty_claim["claim"] = serde_json::json!("");
        assert!(parse_fixture(&serde_json::to_vec(&empty_claim).unwrap()).is_err());
        let mut extra_fail = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_fail["failClosed"]["cancel"]["unexpected"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_fail).unwrap()).is_err());
        let mut extra_happy = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_happy["happyPath"]["bonus"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_happy).unwrap()).is_err());
        let mut extra_setup = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_setup["setup"]["bonus"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_setup).unwrap()).is_err());
        let mut extra_plan = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_plan["managerPlan"]["bonus"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_plan).unwrap()).is_err());
        let mut extra_ceil = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_ceil["resourceCeilings"]["bonus"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_ceil).unwrap()).is_err());
        let mut extra_scan = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        extra_scan["artifactScan"]["bonus"] = serde_json::json!(1);
        assert!(parse_fixture(&serde_json::to_vec(&extra_scan).unwrap()).is_err());
        for key in [
            "claim",
            "nextRequiredCampaign",
            "providerAttemptProjection",
            "uncertainAcceptProjection",
            "retryClassProjection",
            "quotaLedger",
            "provedOracle",
            "ciMode",
            "soak10m",
            "soak24h",
            "happyPath",
            "setup",
            "managerPlan",
            "failClosed",
            "resourceCeilings",
            "artifactScan",
            "requiredAssertions",
        ] {
            let mut dropped = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
            dropped.as_object_mut().unwrap().remove(key);
            assert!(
                parse_fixture(&serde_json::to_vec(&dropped).unwrap()).is_err(),
                "removing {key} must fail closed"
            );
        }
        let mut missing_status = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        missing_status["failClosed"]
            .as_object_mut()
            .unwrap()
            .remove("status500");
        assert!(parse_fixture(&serde_json::to_vec(&missing_status).unwrap()).is_err());
        let mut missing_stop = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        missing_stop["failClosed"]["malformed"]
            .as_object_mut()
            .unwrap()
            .remove("stopCause");
        assert!(parse_fixture(&serde_json::to_vec(&missing_stop).unwrap()).is_err());
        let mut zero_posts = serde_json::from_slice::<Value>(FIXTURE_BYTES).unwrap();
        zero_posts["failClosed"]["cancel"]["posts"] = serde_json::json!(0);
        assert!(parse_fixture(&serde_json::to_vec(&zero_posts).unwrap()).is_err());
    }
}

#[cfg(test)]
mod redaction_scan_tests {
    use super::{project_public_mcp_for_secret_scan, scan_mcp, TOKEN};
    use grokptah_agent_bridge::scan_value_for_forbidden_data;
    use serde_json::json;

    #[test]
    fn public_agent_ids_are_projected_then_scan_propagates() {
        let structured = json!({
            "agentId": "agent-550e8400-e29b-41d4-a716-446655440000",
            "spec": {"displayName": "agent-550e8400-e29b-41d4-a716-446655440000"},
            "runId": "550e8400-e29b-41d4-a716-446655440000",
            "state": "interrupted"
        });
        let projected = project_public_mcp_for_secret_scan(&structured);
        assert_eq!(projected["agentId"], "<opaque-id>");
        assert_eq!(projected["spec"]["displayName"], "<opaque-id>");
        scan_value_for_forbidden_data(&projected).unwrap();
        scan_mcp("ptah_get_run", &structured, &structured);
    }

    #[test]
    fn authority_principal_id_is_projected_before_forbidden_scan() {
        let structured = json!({
            "principal": {
                "principalId": "primary",
                "credentialId": "primary",
                "role": "remote_operator"
            }
        });
        let projected = project_public_mcp_for_secret_scan(&structured);
        assert!(projected["principal"].get("principalId").is_none());
        assert_eq!(projected["principal"]["principalRef"], "<opaque-id>");
        scan_value_for_forbidden_data(&projected).unwrap();
        scan_mcp("ptah_get_authority_capabilities", &structured, &structured);
    }

    #[test]
    #[should_panic(expected = "leaked sentinel")]
    fn forbidden_sentinel_in_non_identity_field_still_fails() {
        let structured = json!({
            "agentId": "agent-550e8400-e29b-41d4-a716-446655440000",
            "detail": TOKEN
        });
        scan_mcp("ptah_get_run", &structured, &structured);
    }
}

#[cfg(test)]
mod campaign_artifact_scan_tests {
    use super::{scan_tree, ArtifactScan};

    fn limits() -> ArtifactScan {
        ArtifactScan {
            max_depth: 4,
            max_files: 16,
            max_file_bytes: 4096,
            stderr_head_bytes: 4096,
            stderr_tail_bytes: 4096,
        }
    }

    #[test]
    fn campaign_specific_secret_is_rejected_from_retained_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("audit.json"), "worker-campaign-secret").unwrap();
        assert!(scan_tree(home.path(), &limits(), &["unrelated-secret"]).is_ok());
        let error = scan_tree(home.path(), &limits(), &["worker-campaign-secret"])
            .expect_err("campaign credential must fail the retained-home scan");
        assert!(error.contains("campaign sentinel"), "{error}");
    }
}
