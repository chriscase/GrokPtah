//! Versioned public-boundary desktop/hosted black-box parity fixture v1.
//!
//! After launch, the scenario may interact only through `McpControlClient`
//! initialize/list_tools/call_tool/close_session. Launch handles are retained
//! only for stop/restart. Fake transport (MockGateway) is inspected for
//! request cardinalities without exposing secrets.
//!
//! Timing (do not claim Utc timestamps are fake-controlled):
//! - Both adapters are in-process on this test's Tokio runtime. Hosted
//!   `start_service` is not a child process.
//! - `#[tokio::test(start_paused = true)]` drives in-process timers via
//!   bounded `tokio::time::advance` plus yield loops. No wall-clock sleeps.
//! - `MockGateway::stall` holds the accepted connection with
//!   `std::future::pending()` after the request is recorded and before any
//!   response bytes. Paused time cannot complete a stall.
//! - Durable supervisor OS threads still use wall-clock time; manager
//!   progress is driven through public `ptah_tick_manager_plan`.
//! - `chrono::Utc` timestamps are real and are stripped as transport-volatile.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, AgentHost,
    AgentHostHandle, ControlServerHandle, HostConfig, McpControlClient, McpRemoteError,
    OrchestrationConfig, OrchestrationService, RuntimeHome, RuntimeHostKind, WorkspaceAllowlist,
};
use grokptah_service::{start_service, ServiceConfig, ServiceHandle};
use grokptah_test_gateway::{MockGateway, RecordedRequest, Response, Step};
use serde_json::{json, Map, Value};
use tempfile::TempDir;

const FIXTURE_SCHEMA: &str = "grokptah.shared-black-box-fixture.v1";
const RESULT_SCHEMA: &str = "grokptah.shared-black-box-result.v1";
const EXPECTED_COMMON_HOST_CAPABILITIES: &[&str] = &[
    "durable_runs",
    "durable_sessions",
    "durable_work",
    "event_replay",
    "native_execution",
    "persistent_agents",
    "routines",
];
const EXPECTED_DESKTOP_LOCAL_HOST_CAPABILITIES: &[&str] = &[
    "desktop_keychain",
    "desktop_local_approval",
    "desktop_pty",
    "semantic_computer_use_foreground",
];
const SCENARIO_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/shared-black-box/v1/scenario.json"
);
const GOLDEN_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/shared-black-box/v1"
);

/// Compile-time map from audited host revision to the one immutable golden.
/// Never infer the golden from advertised tools or feature downgrade.
const AUDITED_GOLDENS: &[(&str, &str)] = &[
    (
        "4bd2081b2945e8ce881895f976bb7c8d88b929f2",
        "expected-pr352-4bd2081b.json",
    ),
    (
        "67e29bd34dc64049432c715c93c2cef2185c63ea",
        "expected-main-67e29bd3.json",
    ),
];

const FIXTURE_ALLOWLIST: &[&str] = &[
    "crates/codegen/grokptah-service/Cargo.toml",
    "crates/codegen/grokptah-service/Cargo.lock",
    "crates/codegen/grokptah-service/tests/shared_black_box_v1.rs",
    "crates/codegen/grokptah-service/tests/common/mod.rs",
    "crates/codegen/grokptah-service/tests/common/shared_black_box_v1.rs",
    "crates/codegen/grokptah-service/tests/fixtures/shared-black-box/v1/scenario.json",
    "crates/codegen/grokptah-service/tests/fixtures/shared-black-box/v1/expected-main-67e29bd3.json",
    "crates/codegen/grokptah-service/tests/fixtures/shared-black-box/v1/expected-pr352-4bd2081b.json",
];

const GOLDEN_UPDATE_ENV_VARS: &[&str] = &[
    "UPDATE_SHARED_BLACK_BOX_GOLDEN",
    "GROKPTAH_UPDATE_GOLDENS",
    "UPDATE_GOLDENS",
    "UPDATE_SNAPSHOTS",
];

/// Set true in the committed tree. Session recording may flip this locally
/// only long enough to dump `/tmp` JSON; it must be true before push.
const PRELOAD_IMMUTABLE_GOLDEN: bool = true;

/// Revision the workflow that compiled this test binary declares it is
/// certifying, as a full commit sha.
///
/// `option_env!` is resolved by rustc while the workflow builds the test, so
/// the value is sealed into the binary. A `std::env::var` read would let
/// anyone who can start the already-built test name any revision they liked;
/// this cannot be redirected by an ordinary runtime caller.
///
/// A declaration is never trusted on its own. It only survives
/// [`verify_declared_source_revision`], which re-derives the claim from the
/// repository, so a stale or wrong declaration fails closed rather than
/// selecting someone else's golden.
const DECLARED_SOURCE_REVISION: Option<&str> =
    option_env!("GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION");

/// Serializes process-global environment mutations for this test binary.
pub struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(OsString, Option<OsString>)>,
}

impl ProcessEnvGuard {
    pub fn new() -> Self {
        Self {
            _lock: home_override_serial(),
            previous: Vec::new(),
        }
    }

    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let key = key.as_ref();
        self.remember(key);
        unsafe {
            std::env::set_var(key, value);
        }
    }

    pub fn remove(&mut self, key: impl AsRef<OsStr>) {
        let key = key.as_ref();
        self.remember(key);
        unsafe {
            std::env::remove_var(key);
        }
    }

    fn remember(&mut self, key: &OsStr) {
        self.previous
            .push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        for (key, previous) in self.previous.drain(..).rev() {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum EndpointKind {
    Desktop,
    Hosted,
}

impl EndpointKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Hosted => "hosted",
        }
    }
}

enum EndpointInner {
    Desktop {
        server: ControlServerHandle,
        host: AgentHostHandle,
    },
    Hosted {
        handle: ServiceHandle,
    },
}

struct Launched {
    mcp: McpControlClient,
    inner: EndpointInner,
    addr: String,
    initialization: Option<Value>,
}

struct Scenario {
    raw: Value,
}

impl Scenario {
    fn load() -> Self {
        let text = std::fs::read_to_string(SCENARIO_PATH).expect("read scenario.json");
        let raw: Value = serde_json::from_str(&text).expect("parse scenario.json");
        assert_eq!(
            raw["schema"].as_str(),
            Some(FIXTURE_SCHEMA),
            "scenario schema"
        );
        Self { raw }
    }

    fn str(&self, path: &[&str]) -> String {
        let mut cur = &self.raw;
        for key in path {
            cur = &cur[*key];
        }
        cur.as_str()
            .unwrap_or_else(|| panic!("scenario string {}", path.join(".")))
            .to_string()
    }

    fn u64(&self, path: &[&str]) -> u64 {
        let mut cur = &self.raw;
        for key in path {
            cur = &cur[*key];
        }
        cur.as_u64()
            .unwrap_or_else(|| panic!("scenario u64 {}", path.join(".")))
    }

    fn bool(&self, path: &[&str]) -> bool {
        let mut cur = &self.raw;
        for key in path {
            cur = &cur[*key];
        }
        cur.as_bool()
            .unwrap_or_else(|| panic!("scenario bool {}", path.join(".")))
    }

    fn request_id(&self, name: &str) -> String {
        self.str(&["requestIds", name])
    }

    fn max_ticks(&self) -> u64 {
        self.u64(&["bounds", "maxLogicalTicks"])
    }

    fn yields(&self) -> u64 {
        self.u64(&["bounds", "maxYieldsPerWait"])
    }

    fn native_ms(&self) -> u64 {
        self.u64(&["bounds", "advanceNativeMs"])
    }

    fn supervisor_ms(&self) -> u64 {
        self.u64(&["bounds", "advanceSupervisorMs"])
    }

    fn golden_selector(&self) -> BTreeMap<String, String> {
        let Some(map) = self.raw["goldenSelector"].as_object() else {
            panic!("scenario.json missing goldenSelector");
        };
        map.iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("goldenSelector.{key} must be a string"))
                        .to_string(),
                )
            })
            .collect()
    }
}

struct RedactionHit {
    path: String,
    reason: String,
}

struct ScanCtx<'a> {
    bearer: &'a str,
    api_key: &'a str,
    gateway: &'a str,
    gateway_authority: &'a str,
    marker: &'a str,
    hits: Vec<RedactionHit>,
}

struct RedactionNeedles<'a> {
    bearer: &'a str,
    api_key: &'a str,
    gateway: &'a str,
    gateway_authority: &'a str,
    marker: &'a str,
}

struct DriveSixPhases<'a> {
    kind: EndpointKind,
    scenario: &'a Scenario,
    home: &'a Path,
    workspace: &'a Path,
    token: &'a str,
    gateway: &'a MockGateway,
    proposal_file: &'a str,
}

struct WorkStateWait<'a> {
    session_id: &'a str,
    workspace: &'a str,
    work_id: &'a str,
    states: &'a [&'a str],
    advance: Duration,
}

struct IdCanon {
    map: BTreeMap<String, String>,
}

struct GatewayScript {
    proposal_turns: Arc<AtomicUsize>,
    restart_objective: String,
    proposal_path: String,
}

pub async fn run_fixture() {
    let scenario = Scenario::load();
    reject_golden_mutation_env();
    let source = resolve_audited_source_revision();
    let golden_name = select_golden_file(&source, &scenario);
    let golden_path = PathBuf::from(GOLDEN_DIR).join(golden_name);
    let golden_before = snapshot_path(&golden_path);
    let expected = if PRELOAD_IMMUTABLE_GOLDEN {
        Some(load_immutable_golden(&golden_path, &source))
    } else {
        eprintln!(
            "shared-black-box-v1 recording: PRELOAD_IMMUTABLE_GOLDEN=false; will dump normalized JSON to temp"
        );
        None
    };

    let mut env = ProcessEnvGuard::new();
    env.remove("GROKPTAH_AGENT_OFFLINE");
    env.remove("XAI_API_KEY");
    env.remove("OPENAI_API_KEY");
    env.remove("OPENAI_BASE_URL");
    env.remove("OPENAI_API_BASE");

    let desktop = run_v1(EndpointKind::Desktop, &scenario, &mut env).await;
    let hosted = run_v1(EndpointKind::Hosted, &scenario, &mut env).await;

    let mut desktop_result = desktop.result.clone();
    let mut hosted_result = hosted.result.clone();
    stamp_source_revision(&mut desktop_result, &source);
    stamp_source_revision(&mut hosted_result, &source);

    let desktop_json = canonical_json(&desktop_result);
    let hosted_json = canonical_json(&hosted_result);
    let desktop_hash = sha256_hex(desktop_json.as_bytes());
    let hosted_hash = sha256_hex(hosted_json.as_bytes());
    eprintln!("shared-black-box-v1 audited source revision={source}");
    eprintln!("shared-black-box-v1 selected golden={golden_name}");
    eprintln!("shared-black-box-v1 desktop normalized sha256={desktop_hash}");
    eprintln!("shared-black-box-v1 hosted normalized sha256={hosted_hash}");
    eprintln!(
        "shared-black-box-v1 desktop transport {:?}",
        desktop_result["transport"]
    );
    eprintln!(
        "shared-black-box-v1 hosted transport {:?}",
        hosted_result["transport"]
    );
    dump_normalized_temp("desktop", &desktop_result);
    dump_normalized_temp("hosted", &hosted_result);

    let mut report = Vec::new();
    if golden_before != snapshot_path(&golden_path) {
        report.push(format!(
            "immutable golden was rewritten during the run: {}",
            golden_path.display()
        ));
    }
    if desktop_json != hosted_json {
        report.push(format!(
            "desktop and hosted normalized JSON are not byte-equal\ndesktop={desktop_hash}\nhosted={hosted_hash}"
        ));
    }
    if let Some(expected) = expected.as_ref() {
        match compare_normalized(&desktop_result, expected) {
            Ok(()) => {}
            Err(errors) => report.push(format!(
                "selected golden mismatch ({golden_name}):\n{}",
                errors.join("\n")
            )),
        }
    }
    for (label, endpoint) in [("desktop", &desktop), ("hosted", &hosted)] {
        if !endpoint.defects.is_empty() {
            report.push(format!(
                "{label} mutation-resistant assertions failed:\n{}",
                endpoint.defects.join("\n")
            ));
        }
        if !endpoint.redaction_hits.is_empty() {
            report.push(format!(
                "{label} redaction scan failed; paths only (values omitted):\n{}",
                collapse_redaction_hits(&endpoint.redaction_hits).join("\n")
            ));
        }
    }
    if report.is_empty() {
        return;
    }
    panic!("{}", report.join("\n\n"));
}

struct EndpointOutcome {
    result: Value,
    defects: Vec<String>,
    redaction_hits: Vec<String>,
}

fn collapse_redaction_hits(hits: &[String]) -> Vec<String> {
    let mut out: Vec<String> = hits
        .iter()
        .map(|hit| {
            let collapsed: String = hit
                .chars()
                .fold((String::new(), false), |(mut acc, in_index), ch| {
                    if ch == '[' {
                        acc.push_str("[]");
                        (acc, true)
                    } else if ch == ']' {
                        (acc, false)
                    } else if in_index {
                        (acc, true)
                    } else {
                        acc.push(ch);
                        (acc, false)
                    }
                })
                .0;
            collapsed
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

async fn run_v1(
    kind: EndpointKind,
    scenario: &Scenario,
    env: &mut ProcessEnvGuard,
) -> EndpointOutcome {
    let home_dir = TempDir::new().expect("temp home");
    let workspace_dir = TempDir::new().expect("temp workspace");
    let home = dunce::canonicalize(home_dir.path()).expect("canonicalize home");
    let workspace = dunce::canonicalize(workspace_dir.path()).expect("canonicalize workspace");
    env.set("HOME", &home);
    env.set("GROK_HOME", home.join(".grok"));
    std::fs::create_dir_all(home.join(".grok")).expect("grok home");
    // Pin the catalog default so both adapters resolve the fixture modelId
    // instead of grok-4.5 from the builtin preference list.
    std::fs::write(
        home.join(".grok").join("config.toml"),
        format!(
            "[models]\ndefault = \"{}\"\n",
            scenario.str(&["identities", "modelId"])
        ),
    )
    .expect("write grok config.toml");
    set_grokptah_home_override(Some(home.clone()));

    let token = scenario.str(&["secrets", "mcpBearer"]);
    let api_key = scenario.str(&["secrets", "apiKey"]);
    let marker = scenario.str(&["secrets", "privateGatewayMarker"]);
    let restart_objective = scenario.str(&["objectives", "restartCut"]);
    let proposal_path = scenario.str(&["proposal", "targetRelativePath"]);

    let script = GatewayScript {
        proposal_turns: Arc::new(AtomicUsize::new(0)),
        restart_objective: restart_objective.clone(),
        proposal_path: proposal_path.clone(),
    };
    let gateway = start_scripted_gateway(script).await;
    let gateway_base = gateway.base_url().to_string();
    let gateway_authority = gateway_base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string();
    env.set("GROKPTAH_API_BASE", format!("{gateway_base}/v1"));
    env.set("GROKPTAH_API_KEY", &api_key);

    let mut hits = Vec::new();
    let mut launched = start_endpoint(kind, &home, &workspace, &token).await;
    initialize_mcp(&mut launched).await;
    let (result, mut launched, defects) = {
        let mut scan = |value: &Value, origin: &str| {
            scan_raw(
                value,
                origin,
                RedactionNeedles {
                    bearer: &token,
                    api_key: &api_key,
                    gateway: &gateway_base,
                    gateway_authority: &gateway_authority,
                    marker: &marker,
                },
                &mut hits,
            );
        };
        drive_six_phases(
            DriveSixPhases {
                kind,
                scenario,
                home: &home,
                workspace: &workspace,
                token: &token,
                gateway: &gateway,
                proposal_file: &proposal_path,
            },
            launched,
            &mut scan,
        )
        .await
    };

    let _ = launched.mcp.close_session().await;
    stop_endpoint(launched).await;
    drop(gateway);
    drop(home_dir);
    drop(workspace_dir);

    let redaction_hits: Vec<String> = hits
        .iter()
        .map(|hit| format!("{}: {}", hit.path, hit.reason))
        .collect();

    let mut normalized =
        normalize_result(result, &workspace, &home, &gateway_base, &gateway_authority);
    normalized["schema"] = json!(RESULT_SCHEMA);
    normalized["version"] = json!("v1");
    let mut normalized_hits = Vec::new();
    scan_normalized_secrets(
        &normalized,
        &token,
        &api_key,
        &gateway_base,
        &gateway_authority,
        &marker,
        &mut normalized_hits,
    );
    let mut all_hits = redaction_hits;
    all_hits.extend(normalized_hits);
    EndpointOutcome {
        result: normalized,
        defects,
        redaction_hits: all_hits,
    }
}

async fn start_scripted_gateway(script: GatewayScript) -> MockGateway {
    let proposal_turns = script.proposal_turns.clone();
    let restart_objective = script.restart_objective.clone();
    let proposal_path = script.proposal_path.clone();
    MockGateway::start_routed(move |request: &RecordedRequest| {
        if request.path.starts_with("/v1/models") {
            return Step::respond(Response::json_ok(&json!({
                "object": "list",
                "data": [{
                    "id": "grok-build",
                    "object": "model",
                    "created": 1,
                    "owned_by": "xai"
                }]
            })));
        }
        let body = request.body_str();
        if body.contains(&restart_objective) {
            return Step::stall();
        }
        if body.contains("Return exactly this JSON envelope") {
            let turn = proposal_turns.fetch_add(1, Ordering::SeqCst);
            if turn == 0 {
                let arguments = json!({
                    "path": proposal_path,
                    "content": "must never be written\n"
                })
                .to_string();
                return Step::respond(Response::json_ok(&json!({
                    "choices": [{
                        "message": {
                            "content": null,
                            "tool_calls": [{
                                "id": "proposal-write",
                                "type": "function",
                                "function": {
                                    "name": "write_file",
                                    "arguments": arguments
                                }
                            }]
                        }
                    }],
                    "usage": {
                        "prompt_tokens": 6,
                        "completion_tokens": 4,
                        "total_tokens": 10
                    }
                })));
            }
            let denied = body.contains("DENIED by deny rule")
                && body.contains("write_file")
                && body.contains("proposal-write");
            if !denied {
                return Step::respond(Response::json(
                    500,
                    &json!({"error": {"type": "fixture", "message": "proposal second turn without host denial"}}),
                ));
            }
            let Some(envelope) = extract_json_object_after(&body, "Envelope: ") else {
                return Step::respond(Response::json(
                    500,
                    &json!({"error": {"type": "fixture", "message": "missing exact envelope substring"}}),
                ));
            };
            return Step::respond(Response::json_ok(&json!({
                "choices": [{
                    "message": { "content": envelope }
                }],
                "usage": {
                    "prompt_tokens": 6,
                    "completion_tokens": 4,
                    "total_tokens": 10
                }
            })));
        }
        Step::respond(Response::json_ok(&json!({
            "choices": [{
                "message": { "content": "native route snapshot verified" }
            }],
            "usage": {
                "prompt_tokens": 6,
                "completion_tokens": 4,
                "total_tokens": 10
            }
        })))
    })
    .await
}

async fn start_endpoint(
    kind: EndpointKind,
    home: &Path,
    workspace: &Path,
    token: &str,
) -> Launched {
    match kind {
        EndpointKind::Desktop => {
            let runtime_home = RuntimeHome::from_path(home).expect("desktop runtime home");
            let host = AgentHost::create_with_runtime_home(HostConfig::default(), runtime_home);
            host.start().expect("start desktop agent host");
            let store = host
                .ensure_orchestration_store()
                .expect("desktop orchestration store");
            let orch = OrchestrationService::new_for_host(
                host.clone(),
                host.event_bus(),
                store,
                OrchestrationConfig {
                    bearer_token: token.to_string(),
                    allowlist: WorkspaceAllowlist::new([workspace.to_path_buf()]),
                    max_concurrent_runs: 4,
                    bounds: Default::default(),
                },
                RuntimeHostKind::DesktopLocal,
            );
            let server = start_control_server(orch, 0)
                .await
                .expect("desktop control server");
            let addr = format!("http://{}", server.addr);
            let mcp = McpControlClient::new(&addr, token);
            Launched {
                mcp,
                inner: EndpointInner::Desktop { server, host },
                addr,
                initialization: None,
            }
        }
        EndpointKind::Hosted => {
            let config = ServiceConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                token,
                vec![workspace.to_path_buf()],
                false,
                4,
                Duration::from_secs(120),
            )
            .expect("hosted service config")
            .with_runtime_home(home)
            .expect("hosted runtime home");
            let handle = start_service(config).await.expect("start hosted service");
            let addr = format!("http://{}", handle.addr);
            let mcp = McpControlClient::new(&addr, token);
            Launched {
                mcp,
                inner: EndpointInner::Hosted { handle },
                addr,
                initialization: None,
            }
        }
    }
}

async fn stop_endpoint(launched: Launched) {
    match launched.inner {
        EndpointInner::Desktop { server, host } => {
            server.stop_and_wait().await;
            let _ = host.stop();
        }
        EndpointInner::Hosted { handle } => {
            handle.stop_and_wait().await;
        }
    }
}

async fn restart_endpoint(
    kind: EndpointKind,
    launched: Launched,
    home: &Path,
    workspace: &Path,
    token: &str,
) -> Launched {
    let _ = launched.mcp;
    stop_endpoint(launched).await;
    yield_budget(16).await;
    let mut next = start_endpoint(kind, home, workspace, token).await;
    initialize_mcp(&mut next).await;
    next
}

async fn initialize_mcp(launched: &mut Launched) {
    yield_budget(16).await;
    let initialization = launched
        .mcp
        .initialize()
        .await
        .unwrap_or_else(|error| panic!("initialize {}: {error}", launched.addr));
    launched.initialization = Some(initialization);
}

fn authority_capability_document(initialization: &Value) -> Value {
    initialization["_meta"]["grokptah/authorityCapabilities"].clone()
}

fn host_capability_contract(
    kind: EndpointKind,
    document: &Value,
    defects: &mut Vec<String>,
) -> Value {
    let expected_kind = match kind {
        EndpointKind::Desktop => "desktop_local",
        EndpointKind::Hosted => "standalone_service",
    };
    let mut expected_capabilities = EXPECTED_COMMON_HOST_CAPABILITIES
        .iter()
        .map(|value| (*value).to_string())
        .collect::<Vec<_>>();
    if matches!(kind, EndpointKind::Desktop) {
        expected_capabilities.extend(
            EXPECTED_DESKTOP_LOCAL_HOST_CAPABILITIES
                .iter()
                .map(|value| (*value).to_string()),
        );
    }
    expected_capabilities.sort();

    let actual_capabilities = document["hostCapabilities"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let expected_hard_denials = vec![
        "approval".to_string(),
        "promotion".to_string(),
        "computer_use".to_string(),
    ];
    let actual_hard_denials = document["hardDenials"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for (label, actual, expected) in [
        (
            "schema",
            document["schema"].clone(),
            json!("grokptah.authority-capabilities.v1"),
        ),
        ("schemaVersion", document["schemaVersion"].clone(), json!(1)),
        (
            "assertedBy.hostKind",
            document["assertedBy"]["hostKind"].clone(),
            json!(expected_kind),
        ),
        (
            "principal.role",
            document["principal"]["role"].clone(),
            json!("remote_coordinator"),
        ),
        (
            "hostCapabilities",
            json!(actual_capabilities),
            json!(expected_capabilities),
        ),
        (
            "hardDenials",
            json!(actual_hard_denials),
            json!(expected_hard_denials),
        ),
    ] {
        if actual != expected {
            defects.push(format!(
                "{}.hostCapability.{label}: actual={actual} expected={expected}",
                kind.as_str()
            ));
        }
    }

    for (label, value) in [
        ("documentHash", document["documentHash"].as_str()),
        (
            "assertedBy.hostInstanceId",
            document["assertedBy"]["hostInstanceId"].as_str(),
        ),
    ] {
        if !value.is_some_and(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            defects.push(format!(
                "{}.hostCapability.{label}: expected an opaque 64-hex identifier",
                kind.as_str()
            ));
        }
    }
    if document["assertedBy"]["hostVersion"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        defects.push(format!(
            "{}.hostCapability.assertedBy.hostVersion: missing",
            kind.as_str()
        ));
    }

    json!({
        "schema": "grokptah.authority-capabilities.v1",
        "schemaVersion": 1,
        "attemptTimeCapture": true,
        "authorityRole": "remote_coordinator",
        "hostKinds": ["desktop_local", "standalone_service"],
        "commonCapabilities": EXPECTED_COMMON_HOST_CAPABILITIES,
        "desktopLocalCapabilities": EXPECTED_DESKTOP_LOCAL_HOST_CAPABILITIES,
        "remoteHardDenials": ["approval", "promotion", "computer_use"]
    })
}

type ScanFn<'a> = dyn FnMut(&Value, &str) + 'a;

async fn drive_six_phases(
    ctx: DriveSixPhases<'_>,
    mut launched: Launched,
    scan: &mut ScanFn<'_>,
) -> (Value, Launched, Vec<String>) {
    let DriveSixPhases {
        kind,
        scenario,
        home,
        workspace,
        token,
        gateway,
        proposal_file,
    } = ctx;
    let workspace_text = workspace.display().to_string();
    let mut defects: Vec<String> = Vec::new();
    let initial_capability = authority_capability_document(
        launched
            .initialization
            .as_ref()
            .expect("initial MCP capability document"),
    );
    scan(&initial_capability, "initialize.authorityCapabilities");
    let mut host_contract = host_capability_contract(kind, &initial_capability, &mut defects);

    // Phase 1: discovery / readiness through tools/list and optional readiness.
    let tools = launched.mcp.list_tools().await.expect("tools/list");
    let advertised: Vec<String> = {
        let mut names = tools
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    let tools_value = json!(advertised
        .iter()
        .map(|name| json!(name))
        .collect::<Vec<_>>());
    scan(&tools_value, "tools/list");
    let mut capability_tools = initial_capability["tools"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    capability_tools.sort();
    if capability_tools != advertised {
        defects.push(format!(
            "{}.hostCapability.tools: initialize document does not match tools/list",
            kind.as_str()
        ));
    }
    let missing_capability_denial = call_scanned(
        &mut launched.mcp,
        "ptah_start_computer_run",
        json!({}),
        scan,
    )
    .await
    .expect_err("undeclared Computer mutation must fail closed");
    if missing_capability_denial != "forbidden_scope" {
        defects.push(format!(
            "{}.hostCapability.missingCapabilityDenial: actual={} expected=forbidden_scope",
            kind.as_str(),
            missing_capability_denial
        ));
    }
    host_contract["missingCapabilityDenial"] = json!(missing_capability_denial);

    let capacity0 = call_ok(&mut launched.mcp, "ptah_get_capacity", json!({}), scan).await;
    let readiness_supported = advertised
        .iter()
        .any(|name| name == "ptah_get_native_coding_readiness");
    let readiness = if readiness_supported {
        call_ok(
            &mut launched.mcp,
            "ptah_get_native_coding_readiness",
            json!({}),
            scan,
        )
        .await
    } else {
        json!({ "support": "absent" })
    };

    // Phase 2: native admission / quota.
    let session = call_ok(
        &mut launched.mcp,
        "ptah_create_session",
        json!({
            "workspace": workspace_text,
            "title": scenario.str(&["identities", "sessionTitle"])
        }),
        scan,
    )
    .await;
    let session_id = session["sessionId"]
        .as_str()
        .expect("sessionId")
        .to_string();

    let bootstrap = call_ok(
        &mut launched.mcp,
        "ptah_submit_task",
        json!({
            "request_id": scenario.request_id("bootstrapSubmit"),
            "session_id": session_id,
            "workspace": workspace_text,
            "prompt": scenario.str(&["objectives", "bootstrap"])
        }),
        scan,
    )
    .await;
    let bootstrap_run = bootstrap["runId"]
        .as_str()
        .expect("bootstrap runId")
        .to_string();
    wait_run_terminal(
        &mut launched,
        scenario,
        &session_id,
        &workspace_text,
        &bootstrap_run,
        scan,
    )
    .await;

    let agents = call_ok(
        &mut launched.mcp,
        "ptah_list_persistent_agents",
        json!({}),
        scan,
    )
    .await;
    let agent_id = agents["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .find_map(|agent| agent["agentId"].as_str())
        .expect("bootstrap agent")
        .to_string();

    let managed_policy = json!({
        "enabled": scenario.bool(&["policies", "managed", "enabled"]),
        "maxConcurrentRuns": scenario.u64(&["policies", "managed", "maxConcurrentRuns"]),
        "retryEligible": scenario.bool(&["policies", "managed", "retryEligible"]),
        "requiresApprovalBeforeExecution": scenario.bool(&["policies", "managed", "requiresApprovalBeforeExecution"]),
        "allowedWorkKinds": scenario.raw["policies"]["managed"]["allowedWorkKinds"].clone(),
        "bounds": {
            "maxPromptBytes": 65536,
            "maxRounds": 8,
            "maxDurationMs": 300000,
            "maxTotalTokens": scenario.u64(&["policies", "managed", "maxTotalTokens"])
        }
    });
    call_ok(
        &mut launched.mcp,
        "ptah_set_managed_execution",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "agent_id": agent_id,
            "policy": managed_policy
        }),
        scan,
    )
    .await;
    let managed = call_ok(
        &mut launched.mcp,
        "ptah_get_managed_execution",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "agent_id": agent_id
        }),
        scan,
    )
    .await;

    let native_objective = scenario.str(&["objectives", "ordinaryNative"]);
    let native_work = call_ok(
        &mut launched.mcp,
        "ptah_create_work",
        json!({
            "request_id": scenario.request_id("createNativeWork"),
            "session_id": session_id,
            "workspace": workspace_text,
            "kind": "native",
            "objective": native_objective,
            "policy": work_policy(&scenario.raw["policies"]["ordinaryNativeWork"])
        }),
        scan,
    )
    .await;
    let native_work_id = native_work["work"]["workId"]
        .as_str()
        .expect("native workId")
        .to_string();
    call_ok(
        &mut launched.mcp,
        "ptah_assign_work",
        json!({
            "request_id": scenario.request_id("assignNativeWork"),
            "session_id": session_id,
            "workspace": workspace_text,
            "work_id": native_work_id,
            "assigned_agent_id": agent_id
        }),
        scan,
    )
    .await;

    wait_work_state(
        &mut launched,
        scenario,
        WorkStateWait {
            session_id: &session_id,
            workspace: &workspace_text,
            work_id: &native_work_id,
            states: &["succeeded"],
            advance: Duration::from_millis(scenario.native_ms()),
        },
        scan,
    )
    .await;

    let native_get = call_ok(
        &mut launched.mcp,
        "ptah_get_work",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "work_id": native_work_id
        }),
        scan,
    )
    .await;
    let listed_work = call_ok(
        &mut launched.mcp,
        "ptah_list_work",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let intents = call_ok(
        &mut launched.mcp,
        "ptah_list_execution_intents",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let runs = call_ok(
        &mut launched.mcp,
        "ptah_list_runs",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let native_run_id = run_id_for_work(&runs, &intents, &native_work_id);
    let native_get_run = call_ok(
        &mut launched.mcp,
        "ptah_get_run",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "run_id": native_run_id
        }),
        scan,
    )
    .await;
    let capacity_native = call_ok(&mut launched.mcp, "ptah_get_capacity", json!({}), scan).await;
    let native_http = chat_requests_containing(gateway, &native_objective);

    // Phase 3: autonomous manager plan.
    let plan = call_ok(
        &mut launched.mcp,
        "ptah_create_manager_plan",
        json!({
            "request_id": scenario.request_id("createManagerPlan"),
            "session_id": session_id,
            "workspace": workspace_text,
            "manager_agent_id": agent_id,
            "objective": scenario.str(&["objectives", "managerGoal"]),
            "steps": [{
                "stepId": "inspect",
                "kind": "native",
                "objective": "shared-black-box-v1-manager-child"
            }],
            "max_in_flight": scenario.u64(&["policies", "managerPlan", "maxInFlight"]),
            "autonomous": scenario.bool(&["policies", "managerPlan", "autonomous"])
        }),
        scan,
    )
    .await;
    let plan_id = plan["plan"]["planId"].as_str().expect("planId").to_string();
    yield_budget(scenario.yields() as usize).await;
    let child_id = wait_child_work(
        &mut launched,
        scenario,
        &session_id,
        &workspace_text,
        &plan_id,
        scan,
    )
    .await;
    call_ok(
        &mut launched.mcp,
        "ptah_cancel_work",
        json!({
            "request_id": scenario.request_id("cancelChildWork"),
            "session_id": session_id,
            "workspace": workspace_text,
            "work_id": child_id,
            "reason": "drive needs_replan for proposal-only decision"
        }),
        scan,
    )
    .await;

    let decision_work_id = wait_decision_work(
        &mut launched,
        scenario,
        &session_id,
        &workspace_text,
        &plan_id,
        scan,
    )
    .await;
    wait_work_state(
        &mut launched,
        scenario,
        WorkStateWait {
            session_id: &session_id,
            workspace: &workspace_text,
            work_id: &decision_work_id,
            states: &["succeeded", "failed", "cancelled"],
            advance: Duration::from_millis(scenario.native_ms()),
        },
        scan,
    )
    .await;

    let plans = call_ok(
        &mut launched.mcp,
        "ptah_list_manager_plans",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let got_plan = call_ok(
        &mut launched.mcp,
        "ptah_get_manager_plan",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "plan_id": plan_id
        }),
        scan,
    )
    .await;
    let decision_get = call_ok(
        &mut launched.mcp,
        "ptah_get_work",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "work_id": decision_work_id
        }),
        scan,
    )
    .await;
    let intents_manager = call_ok(
        &mut launched.mcp,
        "ptah_list_execution_intents",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let runs_manager = call_ok(
        &mut launched.mcp,
        "ptah_list_runs",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;
    let decision_run_id = run_id_for_work(&runs_manager, &intents_manager, &decision_work_id);
    let decision_run = call_ok(
        &mut launched.mcp,
        "ptah_get_run",
        json!({
            "session_id": session_id,
            "workspace": workspace_text,
            "run_id": decision_run_id
        }),
        scan,
    )
    .await;

    // Phase 4: proposal-only malicious write_file.
    let proposal_abs = workspace.join(proposal_file);
    let file_exists = proposal_abs.exists();
    let proposal_http = chat_requests_containing(gateway, "Return exactly this JSON envelope");
    let denial_http = gateway
        .requests()
        .iter()
        .filter(|request| {
            let body = request.body_str();
            body.contains("Return exactly this JSON envelope")
                && body.contains("DENIED by deny rule")
        })
        .count();
    let permission_requests = decision_run
        .pointer("/aggregates/permissionsRequested")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    let permission_grants = decision_run
        .pointer("/aggregates/permissionsGranted")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);

    // Phase 5: restart cut.
    let restart_objective = scenario.str(&["objectives", "restartCut"]);
    let restart_work = call_ok(
        &mut launched.mcp,
        "ptah_create_work",
        json!({
            "request_id": scenario.request_id("createRestartWork"),
            "session_id": session_id,
            "workspace": workspace_text,
            "kind": "native",
            "objective": restart_objective,
            "policy": work_policy(&scenario.raw["policies"]["restartWork"])
        }),
        scan,
    )
    .await;
    let restart_work_id = restart_work["work"]["workId"]
        .as_str()
        .expect("restart workId")
        .to_string();
    call_ok(
        &mut launched.mcp,
        "ptah_assign_work",
        json!({
            "request_id": scenario.request_id("assignRestartWork"),
            "session_id": session_id,
            "workspace": workspace_text,
            "work_id": restart_work_id,
            "assigned_agent_id": agent_id
        }),
        scan,
    )
    .await;
    // Wait until MockGateway has accepted+recorded the restart request. The
    // routed stall returns Step::stall after recording, so the connection is
    // held before any response bytes. Do not wait for a provider response.
    let saw_restart_send = wait_until(
        scenario,
        Duration::from_millis(scenario.native_ms()),
        || async { chat_requests_containing(gateway, &restart_objective) >= 1 },
    )
    .await;
    assert!(
        saw_restart_send,
        "{} restart-cut did not reach the fake provider before stop",
        kind.as_str()
    );
    let restart_http_before = chat_requests_containing(gateway, &restart_objective);
    assert_eq!(
        restart_http_before,
        1,
        "{} restart-cut must send exactly one provider request before stop",
        kind.as_str()
    );
    let stall_held_after_accept = restart_http_before == 1;

    launched = restart_endpoint(kind, launched, home, workspace, token).await;
    let restarted_capability = authority_capability_document(
        launched
            .initialization
            .as_ref()
            .expect("restarted MCP capability document"),
    );
    scan(
        &restarted_capability,
        "restart.initialize.authorityCapabilities",
    );
    if restarted_capability != initial_capability {
        defects.push(format!(
            "{}.hostCapability: attempt-time document changed across owned restart",
            kind.as_str()
        ));
    }
    for _ in 0..8 {
        tokio::time::advance(Duration::from_millis(scenario.native_ms())).await;
        yield_budget(scenario.yields() as usize).await;
    }

    let post1 = restart_observation(
        &mut launched,
        &session_id,
        &workspace_text,
        &restart_work_id,
        gateway,
        &restart_objective,
        scan,
    )
    .await;
    let post2 = restart_observation(
        &mut launched,
        &session_id,
        &workspace_text,
        &restart_work_id,
        gateway,
        &restart_objective,
        scan,
    )
    .await;
    let gateway_base = gateway.base_url().to_string();
    let gateway_authority = gateway_base
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string();
    let post1_norm = normalize_result(
        post1.clone(),
        workspace,
        home,
        &gateway_base,
        &gateway_authority,
    );
    let post2_norm = normalize_result(
        post2.clone(),
        workspace,
        home,
        &gateway_base,
        &gateway_authority,
    );
    if serde_json::to_string(&post1_norm).unwrap() != serde_json::to_string(&post2_norm).unwrap() {
        defects.push(format!(
            "{} post-restart observations did not converge after stripping declared transport-volatile fields",
            kind.as_str()
        ));
    }

    let restart_http_after = chat_requests_containing(gateway, &restart_objective);
    let listed_after = call_ok(
        &mut launched.mcp,
        "ptah_list_work",
        json!({
            "session_id": session_id,
            "workspace": workspace_text
        }),
        scan,
    )
    .await;

    let native_intent_count = count_matching(&intents["intents"], "workId", &native_work_id);
    let native_run_count = count_runs_for_work(&runs, &intents, &native_work_id);
    let native_attempts = native_get["attempts"].as_array().map(Vec::len).unwrap_or(0);
    let native_provider_attempts =
        u64_or_absent(native_get_run.pointer("/providerExecution/attemptCount"));
    let native_quota_state =
        str_or_absent(native_get_run.pointer("/providerExecution/quota/state"));
    let native_tokens = u64_or_absent(
        native_get_run
            .pointer("/providerExecution/attempts/0/usage/total_tokens")
            .or_else(|| native_get_run.pointer("/providerExecution/attempts/0/usage/totalTokens"))
            .or_else(|| native_get_run.pointer("/aggregates/usage/totalTokens")),
    );
    let native_requests = u64_or_absent(
        native_get_run
            .pointer("/providerExecution/attempts/0/usage/requests")
            .or_else(|| native_get_run.pointer("/aggregates/usage/requests")),
    );

    let decision_intent_count =
        count_matching(&intents_manager["intents"], "workId", &decision_work_id);
    let restart_work_row = find_work(&listed_after, &restart_work_id);
    let restart_attempt_count = post1["work"]["attempts"]
        .as_array()
        .map(Vec::len)
        .unwrap_or(0);
    let restart_intent_count =
        count_matching(&post1["intents"]["intents"], "workId", &restart_work_id);
    let restart_run_count =
        count_runs_for_work(&post1["runs"], &post1["intents"], &restart_work_id);
    let restart_send =
        str_or_absent(post1["getRun"].pointer("/providerExecution/attempts/0/sendCertainty"));
    let restart_retry =
        str_or_absent(post1["getRun"].pointer("/providerExecution/attempts/0/retryClass"));
    let restart_quota = str_or_absent(post1["getRun"].pointer("/providerExecution/quota/state"));
    let restart_queued = restart_work_row.get("state").and_then(Value::as_str) == Some("queued");
    let restart_provider_attempts =
        u64_or_absent(post1["getRun"].pointer("/providerExecution/attemptCount"));

    let ordinary = json!({
        "work": 1,
        "intent": native_intent_count,
        "run": native_run_count,
        "providerAttempt": native_provider_attempts,
        "workAttempts": native_attempts,
        "http": native_http,
        "quotaState": native_quota_state,
        "requests": native_requests,
        "tokens": native_tokens
    });
    let manager_obs = json!({
        "decisionWork": 1,
        "decisionIntent": decision_intent_count,
        "decisionRun": if decision_run_id.is_empty() { 0 } else { 1 },
        "proposalHttp": proposal_http,
        "hostDenials": denial_http,
        "permissionRequests": permission_requests,
        "permissionGrants": permission_grants,
        "filesystemWrites": if file_exists { 1 } else { 0 },
        "purpose": decision_run["purpose"]
    });
    let restart_obs = json!({
        "work": 1,
        "workAttempt": restart_attempt_count,
        "intent": restart_intent_count,
        "run": restart_run_count,
        "providerAttempt": restart_provider_attempts,
        "sendCertainty": restart_send,
        "retryClass": restart_retry,
        "quotaState": restart_quota,
        "http": restart_http_after,
        "queued": restart_queued,
        "stallHeldAfterAccept": stall_held_after_accept,
        "httpBeforeStop": restart_http_before
    });

    push_mismatch(
        &mut defects,
        "ordinary.intent",
        &ordinary["intent"],
        json!(1),
    );
    push_mismatch(&mut defects, "ordinary.run", &ordinary["run"], json!(1));
    push_mismatch(
        &mut defects,
        "ordinary.providerAttempt",
        &ordinary["providerAttempt"],
        json!(1),
    );
    push_mismatch(&mut defects, "ordinary.http", &ordinary["http"], json!(1));
    push_mismatch(
        &mut defects,
        "ordinary.tokens",
        &ordinary["tokens"],
        json!(10),
    );
    push_mismatch(
        &mut defects,
        "ordinary.requests",
        &ordinary["requests"],
        json!(1),
    );
    push_mismatch(
        &mut defects,
        "ordinary.quotaState",
        &ordinary["quotaState"],
        json!("consumed"),
    );
    push_mismatch(
        &mut defects,
        "manager.proposalHttp",
        &manager_obs["proposalHttp"],
        json!(2),
    );
    push_mismatch(
        &mut defects,
        "manager.hostDenials",
        &manager_obs["hostDenials"],
        json!(1),
    );
    push_mismatch(
        &mut defects,
        "manager.permissionRequests",
        &manager_obs["permissionRequests"],
        json!(0),
    );
    push_mismatch(
        &mut defects,
        "manager.permissionGrants",
        &manager_obs["permissionGrants"],
        json!(0),
    );
    push_mismatch(
        &mut defects,
        "manager.filesystemWrites",
        &manager_obs["filesystemWrites"],
        json!(0),
    );
    push_mismatch(
        &mut defects,
        "manager.purpose",
        &manager_obs["purpose"],
        json!("manager_proposal"),
    );
    push_mismatch(
        &mut defects,
        "restart.workAttempt",
        &restart_obs["workAttempt"],
        json!(1),
    );
    push_mismatch(
        &mut defects,
        "restart.intent",
        &restart_obs["intent"],
        json!(1),
    );
    push_mismatch(&mut defects, "restart.run", &restart_obs["run"], json!(1));
    push_mismatch(
        &mut defects,
        "restart.providerAttempt",
        &restart_obs["providerAttempt"],
        json!(1),
    );
    push_mismatch(&mut defects, "restart.http", &restart_obs["http"], json!(1));
    push_mismatch(
        &mut defects,
        "restart.stallHeldAfterAccept",
        &restart_obs["stallHeldAfterAccept"],
        json!(true),
    );
    push_mismatch(
        &mut defects,
        "restart.queued",
        &restart_obs["queued"],
        json!(false),
    );
    if restart_obs["sendCertainty"] != "uncertain_accept"
        && restart_obs["sendCertainty"] != "UncertainAccept"
    {
        defects.push(format!(
            "restart.sendCertainty: actual={} expected=uncertain_accept",
            restart_obs["sendCertainty"]
        ));
    }
    if restart_obs["retryClass"] != "explicit_new_run_only"
        && restart_obs["retryClass"] != "ExplicitNewRunOnly"
    {
        defects.push(format!(
            "restart.retryClass: actual={} expected=explicit_new_run_only",
            restart_obs["retryClass"]
        ));
    }
    push_mismatch(
        &mut defects,
        "restart.quotaState",
        &restart_obs["quotaState"],
        json!("reserved"),
    );
    let leak_paths = leak_paths(&post1["getRun"]);
    if !leak_paths.is_empty() {
        defects.push(format!(
            "public get_run leaked secret-bearing fields (paths only): {}",
            leak_paths.join(", ")
        ));
    }

    let result = json!({
        "schema": RESULT_SCHEMA,
        "version": "v1",
        "advertisedTools": advertised,
        "features": {
            "hostCapabilityContract": host_contract,
            "nativeCodingReadiness": if readiness_supported {
                json!({ "support": "present" })
            } else {
                json!({ "support": "absent" })
            }
        },
        "observations": {
            "discovery": {
                "capacity": capacity0,
                "readiness": readiness
            },
            "native": {
                "session": session,
                "managed": managed,
                "work": native_get,
                "listWork": listed_work,
                "intents": intents,
                "runs": runs,
                "getRun": native_get_run,
                "capacity": capacity_native
            },
            "manager": {
                "create": plan,
                "list": plans,
                "get": got_plan,
                "childWorkId": child_id,
                "decisionWork": decision_get,
                "decisionRun": decision_run
            },
            "proposal": {
                "targetRelativePath": proposal_file,
                "fileExists": file_exists,
                "hostDenials": denial_http
            },
            "restart": {
                "httpBeforeStop": restart_http_before,
                "stallHeldAfterAccept": stall_held_after_accept,
                "postRestart": [post1, post2]
            }
        },
        "assertions": {
            "ordinaryNative": ordinary,
            "manager": manager_obs,
            "restart": restart_obs
        },
        "transport": {
            "ordinaryNativeHttp": native_http,
            "proposalHttp": proposal_http,
            "restartHttp": restart_http_after,
            "headerCardinalities": header_cardinalities(gateway)
        }
    });
    (result, launched, defects)
}

async fn restart_observation(
    launched: &mut Launched,
    session_id: &str,
    workspace: &str,
    restart_work_id: &str,
    gateway: &MockGateway,
    restart_objective: &str,
    scan: &mut ScanFn<'_>,
) -> Value {
    let work = call_ok(
        &mut launched.mcp,
        "ptah_get_work",
        json!({
            "session_id": session_id,
            "workspace": workspace,
            "work_id": restart_work_id
        }),
        scan,
    )
    .await;
    let intents = call_ok(
        &mut launched.mcp,
        "ptah_list_execution_intents",
        json!({
            "session_id": session_id,
            "workspace": workspace
        }),
        scan,
    )
    .await;
    let runs = call_ok(
        &mut launched.mcp,
        "ptah_list_runs",
        json!({
            "session_id": session_id,
            "workspace": workspace
        }),
        scan,
    )
    .await;
    let run_id = run_id_for_work(&runs, &intents, restart_work_id);
    let get_run = call_ok(
        &mut launched.mcp,
        "ptah_get_run",
        json!({
            "session_id": session_id,
            "workspace": workspace,
            "run_id": run_id
        }),
        scan,
    )
    .await;
    let capacity = call_ok(&mut launched.mcp, "ptah_get_capacity", json!({}), scan).await;
    json!({
        "work": work,
        "intents": intents,
        "runs": runs,
        "getRun": get_run,
        "capacity": capacity,
        "http": chat_requests_containing(gateway, restart_objective)
    })
}

fn count_matching(arr: &Value, field: &str, expected: &str) -> usize {
    arr.as_array()
        .map(|items| items.iter().filter(|item| item[field] == expected).count())
        .unwrap_or(0)
}

fn count_runs_for_work(runs: &Value, intents: &Value, work_id: &str) -> usize {
    let ids: Vec<String> = intents["intents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|intent| intent["workId"] == work_id)
        .filter_map(|intent| intent["runId"].as_str().map(str::to_string))
        .collect();
    if ids.is_empty() {
        return 0;
    }
    runs["runs"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter(|run| {
                    run["runId"]
                        .as_str()
                        .is_some_and(|id| ids.iter().any(|want| want == id))
                })
                .count()
        })
        .unwrap_or(0)
}

fn find_work(listed: &Value, work_id: &str) -> Value {
    listed["work"]
        .as_array()
        .and_then(|items| items.iter().find(|item| item["workId"] == work_id))
        .cloned()
        .unwrap_or(Value::Null)
}

fn work_policy(node: &Value) -> Value {
    json!({
        "bounds": node["bounds"],
        "retry": node["retry"],
        "requiresApproval": false,
        "maxConcurrentAttempts": 1
    })
}

fn chat_requests_containing(gateway: &MockGateway, needle: &str) -> usize {
    gateway
        .requests()
        .iter()
        .filter(|request| {
            request.path.contains("chat/completions") && request.body_str().contains(needle)
        })
        .count()
}

fn header_cardinalities(gateway: &MockGateway) -> Value {
    let mut rows = Vec::new();
    for request in gateway.requests() {
        if !request.path.contains("chat/completions") {
            continue;
        }
        let mut names: BTreeMap<String, usize> = BTreeMap::new();
        let mut authorization_present = false;
        for (name, _) in &request.headers {
            let key = name.to_ascii_lowercase();
            if key == "authorization" {
                authorization_present = true;
            }
            *names.entry(key).or_insert(0) += 1;
        }
        rows.push(json!({
            "path": "/v1/chat/completions",
            "method": request.method,
            "headerNames": names,
            "authorizationPresent": authorization_present,
            "bodyLen": request.body.len()
        }));
    }
    json!(rows)
}

async fn call_ok(
    mcp: &mut McpControlClient,
    name: &str,
    arguments: Value,
    scan: &mut ScanFn<'_>,
) -> Value {
    match mcp.call_tool(name, arguments.clone()).await {
        Ok(result) => {
            scan(&result.raw, name);
            scan(&result.structured, name);
            assert!(!result.is_error, "{name} returned isError: {}", result.raw);
            result.structured
        }
        Err(error) => {
            if let Some(remote) = error.downcast_ref::<McpRemoteError>() {
                let code = remote.data_code().unwrap_or("unknown");
                let wrapped = json!({ "mcpError": { "code": code } });
                scan(&wrapped, name);
                panic!("{name} MCP error code={code} error={error} args={arguments}");
            }
            panic!("{name}: {error}");
        }
    }
}

async fn call_scanned(
    mcp: &mut McpControlClient,
    name: &str,
    arguments: Value,
    scan: &mut ScanFn<'_>,
) -> Result<Value, String> {
    match mcp.call_tool(name, arguments).await {
        Ok(result) => {
            scan(&result.raw, name);
            scan(&result.structured, name);
            if result.is_error {
                return Err(format!("{name} isError"));
            }
            Ok(result.structured)
        }
        Err(error) => {
            if let Some(remote) = error.downcast_ref::<McpRemoteError>() {
                let code = remote.data_code().unwrap_or("unknown");
                scan(&json!({ "mcpError": { "code": code } }), name);
                return Err(code.to_string());
            }
            Err(error.to_string())
        }
    }
}

async fn yield_budget(n: usize) {
    for _ in 0..n {
        tokio::task::yield_now().await;
    }
}

async fn wait_until<F, Fut>(scenario: &Scenario, advance: Duration, mut pred: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..scenario.max_ticks() {
        yield_budget(scenario.yields() as usize).await;
        if pred().await {
            return true;
        }
        tokio::time::advance(advance).await;
    }
    yield_budget(scenario.yields() as usize).await;
    pred().await
}

async fn wait_run_terminal(
    launched: &mut Launched,
    scenario: &Scenario,
    session_id: &str,
    workspace: &str,
    run_id: &str,
    scan: &mut ScanFn<'_>,
) {
    let advance = Duration::from_millis(scenario.native_ms());
    for _ in 0..scenario.max_ticks() {
        yield_budget(scenario.yields() as usize).await;
        let run = call_ok(
            &mut launched.mcp,
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id
            }),
            scan,
        )
        .await;
        if matches!(
            run["state"].as_str(),
            Some("completed" | "failed" | "cancelled" | "interrupted" | "limit_reached")
        ) {
            return;
        }
        tokio::time::advance(advance).await;
    }
    panic!("bootstrap/run {run_id} did not become terminal");
}

async fn wait_work_state(
    launched: &mut Launched,
    scenario: &Scenario,
    wait: WorkStateWait<'_>,
    scan: &mut ScanFn<'_>,
) {
    let WorkStateWait {
        session_id,
        workspace,
        work_id,
        states,
        advance,
    } = wait;
    for _ in 0..scenario.max_ticks() {
        yield_budget(scenario.yields() as usize).await;
        let snap = call_ok(
            &mut launched.mcp,
            "ptah_get_work",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "work_id": work_id
            }),
            scan,
        )
        .await;
        if snap["work"]["state"]
            .as_str()
            .is_some_and(|state| states.contains(&state))
        {
            return;
        }
        tokio::time::advance(advance).await;
    }
    panic!("work {work_id} did not reach {states:?}");
}

async fn wait_child_work(
    launched: &mut Launched,
    scenario: &Scenario,
    session_id: &str,
    workspace: &str,
    plan_id: &str,
    scan: &mut ScanFn<'_>,
) -> String {
    let advance = Duration::from_millis(scenario.supervisor_ms());
    let mut tick_seq = 0u64;
    for _ in 0..scenario.max_ticks() {
        yield_budget(scenario.yields() as usize).await;
        wake_manager_plan(
            launched,
            scenario,
            session_id,
            workspace,
            plan_id,
            &mut tick_seq,
            scan,
        )
        .await;
        let listed = call_ok(
            &mut launched.mcp,
            "ptah_list_work",
            json!({
                "session_id": session_id,
                "workspace": workspace
            }),
            scan,
        )
        .await;
        if let Some(id) = listed["work"].as_array().and_then(|items| {
            items.iter().find_map(|item| {
                if item["kind"] == "native"
                    && item["sourceManagerPlanId"] == plan_id
                    && item["sourceManagerStepId"] != "__manager_decision__"
                {
                    item["workId"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
        }) {
            return id;
        }
        tokio::time::advance(advance).await;
    }
    panic!("manager child Work was not created");
}

async fn wait_decision_work(
    launched: &mut Launched,
    scenario: &Scenario,
    session_id: &str,
    workspace: &str,
    plan_id: &str,
    scan: &mut ScanFn<'_>,
) -> String {
    let advance = Duration::from_millis(scenario.supervisor_ms());
    let mut tick_seq = 1_000u64;
    for _ in 0..scenario.max_ticks() {
        yield_budget(scenario.yields() as usize).await;
        wake_manager_plan(
            launched,
            scenario,
            session_id,
            workspace,
            plan_id,
            &mut tick_seq,
            scan,
        )
        .await;
        let listed = call_ok(
            &mut launched.mcp,
            "ptah_list_work",
            json!({
                "session_id": session_id,
                "workspace": workspace
            }),
            scan,
        )
        .await;
        if let Some(id) = listed["work"].as_array().and_then(|items| {
            items.iter().find_map(|item| {
                if item["kind"] == "manager-decision"
                    || item["sourceManagerStepId"] == "__manager_decision__"
                {
                    item["workId"].as_str().map(str::to_string)
                } else {
                    None
                }
            })
        }) {
            return id;
        }
        tokio::time::advance(advance).await;
    }
    panic!("manager-decision Work was not created");
}

/// Public-boundary supervisor wake. Errors are scanned and ignored so a stale
/// concurrent supervisor pass cannot abort the fixture.
async fn wake_manager_plan(
    launched: &mut Launched,
    scenario: &Scenario,
    session_id: &str,
    workspace: &str,
    plan_id: &str,
    tick_seq: &mut u64,
    scan: &mut ScanFn<'_>,
) {
    *tick_seq += 1;
    let request_id = format!("{}-{}", scenario.request_id("tickManagerPlan"), tick_seq);
    let _ = call_scanned(
        &mut launched.mcp,
        "ptah_tick_manager_plan",
        json!({
            "request_id": request_id,
            "session_id": session_id,
            "workspace": workspace,
            "plan_id": plan_id
        }),
        scan,
    )
    .await;
}

fn run_id_for_work(runs: &Value, intents: &Value, work_id: &str) -> String {
    if let Some(id) = intents["intents"].as_array().and_then(|items| {
        items.iter().find_map(|intent| {
            if intent["workId"] == work_id {
                intent["runId"].as_str().map(str::to_string)
            } else {
                None
            }
        })
    }) {
        return id;
    }
    runs["runs"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .rev()
                .find_map(|run| run["runId"].as_str().map(str::to_string))
        })
        .expect("run id for work")
}

fn extract_json_object_after<'a>(haystack: &'a str, marker: &str) -> Option<&'a str> {
    let start = haystack.find(marker)? + marker.len();
    let rest = haystack[start..].trim_start();
    let bytes = rest.as_bytes();
    if bytes.first() != Some(&b'{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (index, ch) in rest.char_indices() {
        if in_string {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=index]);
                }
            }
            _ => {}
        }
    }
    None
}

const FORBIDDEN_KEYS: &[&str] = &[
    "credentialref",
    "credentialfingerprint",
    "baseurl",
    "apikey",
    "api_key",
    "bearertoken",
];

fn push_mismatch(defects: &mut Vec<String>, name: &str, actual: &Value, expected: Value) {
    if actual != &expected {
        defects.push(format!("{name}: actual={actual} expected={expected}"));
    }
}

fn leak_paths(value: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    collect_leak_paths(value, "getRun", &mut paths);
    paths
}

fn collect_leak_paths(value: &Value, path: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lowered = key.to_ascii_lowercase();
                if FORBIDDEN_KEYS.contains(&lowered.as_str()) {
                    out.push(format!("{path}.{key}"));
                }
                collect_leak_paths(child, &format!("{path}.{key}"), out);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_leak_paths(child, &format!("{path}[{index}]"), out);
            }
        }
        _ => {}
    }
}

fn scan_raw(
    value: &Value,
    origin: &str,
    needles: RedactionNeedles<'_>,
    hits: &mut Vec<RedactionHit>,
) {
    let mut ctx = ScanCtx {
        bearer: needles.bearer,
        api_key: needles.api_key,
        gateway: needles.gateway,
        gateway_authority: needles.gateway_authority,
        marker: needles.marker,
        hits: Vec::new(),
    };
    walk_scan(value, origin, &mut ctx);
    ctx.hits
        .sort_by(|a, b| a.path.cmp(&b.path).then(a.reason.cmp(&b.reason)));
    ctx.hits
        .dedup_by(|a, b| a.path == b.path && a.reason == b.reason);
    hits.append(&mut ctx.hits);
}

fn walk_scan(value: &Value, path: &str, ctx: &mut ScanCtx<'_>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lowered = key.to_ascii_lowercase();
                if lowered == "headernames" {
                    continue;
                }
                if FORBIDDEN_KEYS.contains(&lowered.as_str()) {
                    ctx.hits.push(RedactionHit {
                        path: format!("{path}.{key}"),
                        reason: format!("forbidden key {key}"),
                    });
                }
                walk_scan(child, &format!("{path}.{key}"), ctx);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_scan(child, &format!("{path}[{index}]"), ctx);
            }
        }
        Value::String(text) => {
            scan_text(path, text, ctx);
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                if parsed.is_object() || parsed.is_array() {
                    walk_scan(&parsed, &format!("{path}.json"), ctx);
                }
            }
        }
        _ => {}
    }
}

fn scan_text(path: &str, text: &str, ctx: &mut ScanCtx<'_>) {
    if text.contains(ctx.bearer) {
        ctx.hits.push(RedactionHit {
            path: path.to_string(),
            reason: "mcp bearer sentinel".into(),
        });
    }
    if text.contains(ctx.api_key) {
        ctx.hits.push(RedactionHit {
            path: path.to_string(),
            reason: "api-key sentinel".into(),
        });
    }
    if text.contains(ctx.gateway) || text.contains(ctx.gateway_authority) {
        ctx.hits.push(RedactionHit {
            path: path.to_string(),
            reason: "private gateway sentinel".into(),
        });
    }
    if text.contains(ctx.marker) {
        ctx.hits.push(RedactionHit {
            path: path.to_string(),
            reason: "private gateway marker".into(),
        });
    }
}

fn scan_normalized_secrets(
    value: &Value,
    bearer: &str,
    api_key: &str,
    gateway: &str,
    gateway_authority: &str,
    marker: &str,
    out: &mut Vec<String>,
) {
    let mut hits = Vec::new();
    scan_raw(
        value,
        "normalized",
        RedactionNeedles {
            bearer,
            api_key,
            gateway,
            gateway_authority,
            marker,
        },
        &mut hits,
    );
    out.extend(
        hits.into_iter()
            .map(|hit| format!("normalized {}: {}", hit.path, hit.reason)),
    );
}

fn normalize_result(
    mut value: Value,
    workspace: &Path,
    home: &Path,
    gateway: &str,
    gateway_authority: &str,
) -> Value {
    replace_authorities(&mut value, workspace, home, gateway, gateway_authority);
    strip_ephemerals(&mut value);
    let mut canon = IdCanon {
        map: BTreeMap::new(),
    };
    collect_ids(&value, None, &mut canon);
    apply_ids(&mut value, &canon);
    sort_value(&mut value, None);
    value
}

fn replace_authorities(
    value: &mut Value,
    workspace: &Path,
    home: &Path,
    gateway: &str,
    gateway_authority: &str,
) {
    let workspace_s = workspace.display().to_string();
    let home_s = home.display().to_string();
    visit_strings(value, &mut |text| {
        let mut out = text.replace(&workspace_s, "$WORKSPACE");
        out = out.replace(&home_s, "$HOME");
        out = out.replace(gateway, "$GATEWAY");
        out = out.replace(gateway_authority, "$GATEWAY");
        out
    });
}

fn visit_strings(value: &mut Value, edit: &mut dyn FnMut(&str) -> String) {
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                visit_strings(child, edit);
            }
        }
        Value::Array(items) => {
            for child in items {
                visit_strings(child, edit);
            }
        }
        Value::String(text) => *text = edit(text),
        _ => {}
    }
}

fn strip_ephemerals(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let keys: Vec<String> = map.keys().cloned().collect();
            for k in keys {
                if is_ephemeral_key(&k) {
                    map.remove(&k);
                    continue;
                }
                if let Some(child) = map.get_mut(&k) {
                    strip_ephemerals(child);
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                strip_ephemerals(child);
            }
        }
        _ => {}
    }
}

fn is_ephemeral_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if is_timestamp_key(&lower) {
        return true;
    }
    matches!(
        lower.as_str(),
        "transportsessionid"
            | "mcp-session-id"
            | "sessionidheader"
            | "port"
            | "listenport"
            | "backend"
            | "backendlabel"
            | "skippedmanual"
            | "skippedineligible"
            | "skippedprovidercapacity"
            | "skippedunroutable"
            | "laggedliveevents"
    )
}

fn is_timestamp_key(lower: &str) -> bool {
    if lower == "format" || lower.len() <= 3 {
        return false;
    }
    lower.ends_with("at")
}

fn collect_ids(value: &Value, key: Option<&str>, canon: &mut IdCanon) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            for k in keys {
                collect_ids(&map[k], Some(k), canon);
            }
        }
        Value::Array(items) => {
            let mut items = items.clone();
            if key.is_some_and(is_set_array_key) {
                items.sort_by_key(canonical_sort_key);
            }
            for child in &items {
                collect_ids(child, key, canon);
            }
        }
        Value::String(text) => {
            if should_canonicalize(key, text) && !canon.map.contains_key(text) {
                let next = format!("$ID_{}", canon.map.len() + 1);
                canon.map.insert(text.clone(), next);
            }
            for captured in find_generated_ids(text) {
                if !canon.map.contains_key(&captured) {
                    let next = format!("$ID_{}", canon.map.len() + 1);
                    canon.map.insert(captured, next);
                }
            }
        }
        _ => {}
    }
}

fn apply_ids(value: &mut Value, canon: &IdCanon) {
    let mut pairs: Vec<(String, String)> = canon
        .map
        .iter()
        .map(|(from, to)| (from.clone(), to.clone()))
        .collect();
    pairs.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    let mut prefixes: Vec<(String, String)> = Vec::new();
    for (from, to) in &pairs {
        if looks_like_uuid(from) && from.len() >= 14 {
            let prefix = from[..14].to_string();
            let unique = pairs
                .iter()
                .filter(|(other, _)| looks_like_uuid(other) && other.starts_with(&prefix))
                .count()
                == 1;
            if unique {
                prefixes.push((prefix, to.clone()));
            }
        }
    }
    prefixes.sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
    visit_strings(value, &mut |text| {
        let mut out = text.to_string();
        for (from, to) in &pairs {
            out = out.replace(from, to);
        }
        for (from, to) in &prefixes {
            out = out.replace(from, to);
        }
        out
    });
}

fn should_canonicalize(key: Option<&str>, text: &str) -> bool {
    if text.starts_with("sbb-v1-")
        || text.starts_with("$")
        || text == "__manager_decision__"
        || text == "inspect"
        || text == "env-grokptah"
        || text == "grok-build"
        || text.contains("shared-black-box-v1")
    {
        return false;
    }
    if looks_like_uuid(text) || looks_like_hash(text) {
        return true;
    }
    let Some(lower) = key.map(str::to_ascii_lowercase) else {
        return false;
    };
    if lower == "stepid" || lower == "kind" || lower == "purpose" || lower == "type" {
        return false;
    }
    (lower.ends_with("id") || lower.ends_with("hash") || lower.ends_with("fingerprint"))
        && !text.is_empty()
        && text != "__manager_decision__"
}

fn looks_like_uuid(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 36
        && bytes[8] == b'-'
        && bytes[13] == b'-'
        && bytes[18] == b'-'
        && bytes[23] == b'-'
        && text.chars().all(|ch| ch.is_ascii_hexdigit() || ch == '-')
}

fn looks_like_hash(text: &str) -> bool {
    let len = text.len();
    (len == 32 || len == 64) && text.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn find_generated_ids(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 36 <= bytes.len() {
        let slice = &text[i..i + 36];
        if looks_like_uuid(slice) {
            out.push(slice.to_string());
            i += 36;
            continue;
        }
        i += 1;
    }
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_hexdigit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
            i += 1;
        }
        let len = i - start;
        if len == 32 || len == 64 {
            let bounded_left = start == 0 || !bytes[start - 1].is_ascii_hexdigit();
            let bounded_right = i == bytes.len() || !bytes[i].is_ascii_hexdigit();
            if bounded_left && bounded_right {
                out.push(text[start..i].to_string());
            }
        }
    }
    out
}

fn is_set_array_key(key: &str) -> bool {
    matches!(
        key,
        "work"
            | "runs"
            | "intents"
            | "plans"
            | "tools"
            | "providers"
            | "agents"
            | "sessions"
            | "advertisedTools"
            | "triggeringWorkIds"
            | "triggeringMessageIds"
            | "allowedWorkKinds"
            | "headerNames"
    )
}

fn canonical_sort_key(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            for key in [
                "objective",
                "kind",
                "purpose",
                "promptPreview",
                "name",
                "workId",
                "runId",
                "planId",
                "agentId",
            ] {
                if let Some(Value::String(text)) = map.get(key) {
                    return format!("{key}:{text}");
                }
            }
            serde_json::to_string(value).unwrap_or_default()
        }
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn sort_value(value: &mut Value, key: Option<&str>) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let mut ordered = Map::new();
            for k in keys {
                if let Some(mut child) = map.remove(&k) {
                    sort_value(&mut child, Some(&k));
                    ordered.insert(k, child);
                }
            }
            *map = ordered;
        }
        Value::Array(items) => {
            for child in items.iter_mut() {
                sort_value(child, key);
            }
            if key.is_some_and(is_set_array_key) {
                items.sort_by_key(canonical_sort_key);
            } else if key == Some("attempts") {
                items.sort_by_key(|item| item["ordinal"].as_u64().unwrap_or(0));
            }
        }
        _ => {}
    }
}

fn u64_or_absent(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_u64) {
        Some(number) => json!(number),
        None => json!({ "support": "absent" }),
    }
}

fn str_or_absent(value: Option<&Value>) -> Value {
    match value.and_then(Value::as_str) {
        Some(text) => json!(text),
        None => json!({ "support": "absent" }),
    }
}

fn canonical_json(value: &Value) -> String {
    serde_json::to_string(value).expect("canonical json")
}

fn snapshot_path(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

fn dump_normalized_temp(label: &str, value: &Value) {
    let source = value
        .get("sourceRevision")
        .and_then(Value::as_str)
        .unwrap_or("");
    let document = write_golden_document(value, source);
    let path = std::env::temp_dir().join(format!("sbb-v1-{label}-normalized.json"));
    let pretty = serde_json::to_string_pretty(&document).expect("pretty dump");
    let _ = std::fs::write(&path, format!("{pretty}\n"));
    eprintln!(
        "shared-black-box-v1 dumped {} evidenceHash={} to {}",
        label,
        document
            .get("evidenceHash")
            .and_then(Value::as_str)
            .unwrap_or("missing"),
        path.display()
    );
}

fn stamp_source_revision(value: &mut Value, source: &str) {
    if let Some(map) = value.as_object_mut() {
        map.insert("sourceRevision".into(), json!(source));
    }
    sort_value(value, None);
}

fn reject_golden_mutation_env() {
    for var in GOLDEN_UPDATE_ENV_VARS {
        if std::env::var_os(var).is_some() {
            panic!("{var} cannot rewrite or bypass the immutable golden");
        }
    }
}

fn select_golden_file(source: &str, scenario: &Scenario) -> &'static str {
    let from_const = AUDITED_GOLDENS
        .iter()
        .find(|(sha, _)| *sha == source)
        .map(|(_, file)| *file);
    let selector = scenario.golden_selector();
    let from_scenario = selector.get(source).map(String::as_str);
    match (from_const, from_scenario) {
        (Some(const_file), Some(scenario_file)) if const_file == scenario_file => const_file,
        (Some(const_file), Some(scenario_file)) => panic!(
            "golden selector mismatch for {source}: compile-time={const_file} scenario={scenario_file}"
        ),
        (None, _) | (_, None) => panic!(
            "unexpected source revision {source}; fail closed (no golden inference by feature downgrade)"
        ),
    }
}

fn load_immutable_golden(path: &Path, expected_source: &str) -> Value {
    if !path.exists() {
        panic!("missing immutable golden {}", path.display());
    }
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("read golden {}: {error}", path.display()));
    let mut value: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("malformed golden {}: {error}", path.display()));
    if value.get("calibration") == Some(&json!("pending")) {
        panic!(
            "pending golden is not an immutable oracle: {}",
            path.display()
        );
    }
    if value["schema"] != RESULT_SCHEMA {
        panic!(
            "malformed golden {}: schema={:?} expected={RESULT_SCHEMA}",
            path.display(),
            value.get("schema")
        );
    }
    if value.get("version") != Some(&json!("v1")) {
        panic!("malformed golden {}: missing version v1", path.display());
    }
    sort_value(&mut value, None);
    let stored_hash = value
        .get("evidenceHash")
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut body = value.clone();
    if let Some(map) = body.as_object_mut() {
        map.remove("evidenceHash");
    }
    sort_value(&mut body, None);
    let computed = format!("sha256:{}", sha256_hex(canonical_json(&body).as_bytes()));
    match stored_hash {
        Some(stored) if stored == computed => {}
        Some(stored) => panic!(
            "golden evidenceHash mismatch for {}: stored={stored} computed={computed}",
            path.display()
        ),
        None => panic!("malformed golden {}: missing evidenceHash", path.display()),
    }
    if value.get("sourceRevision").and_then(Value::as_str) != Some(expected_source) {
        panic!(
            "golden sourceRevision {:?} does not match audited {}",
            value.get("sourceRevision"),
            expected_source
        );
    }
    if let Some(map) = value.as_object_mut() {
        map.remove("evidenceHash");
    }
    sort_value(&mut value, None);
    value
}

fn compare_normalized(actual: &Value, expected: &Value) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    collect_semantic_key_diffs(actual, expected, "", &mut errors);
    if canonical_json(actual) != canonical_json(expected) && errors.is_empty() {
        errors.push(format!(
            "canonical JSON differs (actual sha256={} expected sha256={})",
            sha256_hex(canonical_json(actual).as_bytes()),
            sha256_hex(canonical_json(expected).as_bytes())
        ));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_semantic_key_diffs(
    actual: &Value,
    expected: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    match (actual, expected) {
        (Value::Object(actual_map), Value::Object(expected_map)) => {
            for key in expected_map.keys() {
                if !actual_map.contains_key(key) {
                    errors.push(format!(
                        "{}: missing semantic key {key}",
                        display_path(path)
                    ));
                }
            }
            for key in actual_map.keys() {
                if !expected_map.contains_key(key) {
                    errors.push(format!("{}: extra semantic key {key}", display_path(path)));
                }
            }
            for (key, expected_child) in expected_map {
                if let Some(actual_child) = actual_map.get(key) {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    collect_semantic_key_diffs(actual_child, expected_child, &child_path, errors);
                }
            }
        }
        (Value::Array(actual_items), Value::Array(expected_items)) => {
            if actual_items.len() != expected_items.len() {
                errors.push(format!(
                    "{}: array cardinality actual={} expected={}",
                    display_path(path),
                    actual_items.len(),
                    expected_items.len()
                ));
            }
            for (index, (actual_child, expected_child)) in
                actual_items.iter().zip(expected_items.iter()).enumerate()
            {
                collect_semantic_key_diffs(
                    actual_child,
                    expected_child,
                    &format!("{path}[{index}]"),
                    errors,
                );
            }
        }
        (actual, expected) if actual != expected => {
            errors.push(format!(
                "{}: value actual={actual} expected={expected}",
                display_path(path)
            ));
        }
        _ => {}
    }
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "$".into()
    } else {
        path.to_string()
    }
}

fn repo_root() -> PathBuf {
    dunce::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .expect("canonicalize repo root")
}

fn git_stdout(args: &[&str]) -> Result<String, String> {
    let root = repo_root();
    let output = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string())
}

fn porcelain_paths(status: &str) -> Vec<String> {
    status
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let rest = if line.len() >= 3 && line.as_bytes().get(2) == Some(&b' ') {
                &line[3..]
            } else {
                line.trim()
            };
            rest.split(" -> ").last().unwrap_or(rest).trim().to_string()
        })
        .collect()
}

fn allowlisted(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    if FIXTURE_ALLOWLIST.contains(&path) {
        return true;
    }
    FIXTURE_ALLOWLIST.iter().any(|allowed| {
        allowed.starts_with(&format!("{path}/")) || path.starts_with(&format!("{allowed}/"))
    })
}

fn commit_changed_files(sha: &str) -> Result<Vec<String>, String> {
    let parent = git_stdout(&["rev-parse", &format!("{sha}^")]);
    let diff = if parent.is_ok() {
        git_stdout(&["diff", "--name-only", &format!("{sha}^"), sha])?
    } else {
        git_stdout(&[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "--root",
            "-r",
            sha,
        ])?
    };
    Ok(diff
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn is_full_commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parent_count(sha: &str) -> usize {
    git_stdout(&["rev-list", "--parents", "-n", "1", sha])
        .map(|line| line.split_whitespace().count().saturating_sub(1))
        .unwrap_or(0)
}

/// Non-fixture paths that differ between two trees. Empty means the two
/// revisions are the same host under test, so one's golden describes the
/// other. Fixture-only differences are the same exemption the history walk
/// already applies, not a new one.
fn host_source_differences(from: &str, to: &str) -> Vec<String> {
    git_stdout(&["diff", "--name-only", from, to])
        .unwrap_or_else(|error| panic!("git diff {from} {to}: {error}"))
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty() && !allowlisted(path))
        .map(str::to_string)
        .collect()
}

/// Re-derives a workflow's declaration from the repository. Every branch here
/// panics rather than returning a fallback: an unverifiable declaration must
/// never reach [`select_golden_file`], because that is where a revision starts
/// being treated as audited.
fn verify_declared_source_revision(declared: &str) -> String {
    if !is_full_commit_sha(declared) {
        panic!(
            "declared source revision {declared:?} is not a full 40-character lowercase commit sha; fail closed"
        );
    }
    let resolved = git_stdout(&["rev-parse", "--verify", "--quiet", &format!("{declared}^{{commit}}")])
        .unwrap_or_else(|error| {
            panic!("declared source revision {declared} is not a commit in this checkout ({error}); fail closed")
        });
    if resolved != declared {
        panic!("declared source revision {declared} resolved to {resolved}; fail closed");
    }
    let head = git_stdout(&["rev-parse", "HEAD"]).expect("git rev-parse HEAD");
    if git_stdout(&["merge-base", "--is-ancestor", declared, &head]).is_err() {
        panic!(
            "declared source revision {declared} is not an ancestor of the checked-out HEAD {head}; fail closed"
        );
    }
    let differences = host_source_differences(declared, &head);
    if !differences.is_empty() {
        panic!(
            "checked-out HEAD {head} differs from declared source revision {declared} outside the fixture allowlist ({differences:?}); fail closed (a merge that changes the host is not the declared revision)"
        );
    }
    declared.to_string()
}

/// Walks back over commits that only touch fixture files to name the revision
/// whose host source is actually under test.
fn walk_back_to_host_revision(head: &str) -> String {
    let mut sha = head.to_string();
    loop {
        let files = commit_changed_files(&sha).expect("commit files");
        if files.iter().any(|path| !allowlisted(path)) {
            return sha;
        }
        match git_stdout(&["rev-parse", &format!("{sha}^")]) {
            Ok(parent) => sha = parent,
            Err(_) => panic!("could not identify audited host revision (fail closed)"),
        }
    }
}

/// Names the revision whose golden may be selected.
///
/// A pull-request checkout is an ephemeral merge of the head into the base.
/// That merge sha is created per run and can never appear in a compile-time
/// audited table, so inferring a golden from it would mean inferring one for a
/// tree nobody audited. The workflow therefore declares which revision it is
/// certifying, and the declaration is verified against this checkout before it
/// is used. With no declaration the historical walk still applies, but a merge
/// commit is refused by name instead of failing later as an "unexpected source
/// revision" that hides why it was unexpected.
fn resolve_audited_source_revision() -> String {
    let status = git_stdout(&["status", "--porcelain"]).expect("git status");
    for path in porcelain_paths(&status) {
        if !allowlisted(&path) {
            panic!("unexpected dirty path outside fixture allowlist: {path}");
        }
    }
    if let Some(declared) = DECLARED_SOURCE_REVISION {
        return verify_declared_source_revision(declared.trim());
    }
    let head = git_stdout(&["rev-parse", "HEAD"]).expect("git rev-parse HEAD");
    let resolved = walk_back_to_host_revision(&head);
    if let Some(message) = merge_refusal_message(&resolved, parent_count(&resolved)) {
        panic!("{message}");
    }
    resolved
}

/// A merge commit is refused by name rather than left to fail later as an
/// anonymous "unexpected source revision". Pull-request CI checks out a merge
/// that is minted per run, so the operator needs to be told to declare the
/// revision, not merely told the sha was not recognised.
fn merge_refusal_message(resolved: &str, parents: usize) -> Option<String> {
    (parents > 1).then(|| {
        format!(
            "checked-out HEAD resolves to merge commit {resolved}; fail closed (build this test with GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION set to the exact revision under test)"
        )
    })
}

fn write_golden_document(result: &Value, source: &str) -> Value {
    let mut document = result.clone();
    stamp_source_revision(&mut document, source);
    let mut body = document.clone();
    if let Some(map) = body.as_object_mut() {
        map.remove("evidenceHash");
    }
    sort_value(&mut body, None);
    let hash = format!("sha256:{}", sha256_hex(canonical_json(&body).as_bytes()));
    if let Some(map) = document.as_object_mut() {
        map.insert("evidenceHash".into(), json!(hash));
    }
    sort_value(&mut document, None);
    document
}

fn sha256_hex(bytes: &[u8]) -> String {
    sha256(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    // FIPS 180-4 SHA-256, fixture reporting only.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut msg = bytes.to_vec();
    let bit_len = (bytes.len() as u64) * 8;
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    let mut h = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut a = h[0];
        let mut b = h[1];
        let mut c = h[2];
        let mut d = h[3];
        let mut e = h[4];
        let mut f = h[5];
        let mut g = h[6];
        let mut hh = h[7];
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    let mut out = [0u8; 32];
    for (i, part) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&part.to_be_bytes());
    }
    out
}

fn sample_oracle_result() -> Value {
    json!({
        "schema": RESULT_SCHEMA,
        "version": "v1",
        "sourceRevision": "4bd2081b2945e8ce881895f976bb7c8d88b929f2",
        "advertisedTools": ["ptah_get_capacity"],
        "features": { "nativeCodingReadiness": { "support": "present" } },
        "observations": {
            "discovery": { "capacity": { "ok": true }, "readiness": { "support": "present" } },
            "native": { "getRun": { "purpose": "native", "state": "completed" } },
            "manager": { "purpose": "manager_proposal" },
            "proposal": { "fileExists": false },
            "restart": {
                "httpBeforeStop": 1,
                "stallHeldAfterAccept": true
            }
        },
        "assertions": {
            "ordinaryNative": {
                "work": 1,
                "intent": 1,
                "run": 1,
                "providerAttempt": 1,
                "http": 1,
                "quotaState": "consumed",
                "requests": 1,
                "tokens": 10
            },
            "manager": {
                "proposalHttp": 2,
                "permissionRequests": 0,
                "permissionGrants": 0
            },
            "restart": {
                "work": 1,
                "workAttempt": 1,
                "intent": 1,
                "run": 1,
                "providerAttempt": 1,
                "sendCertainty": "uncertain_accept",
                "retryClass": "explicit_new_run_only",
                "quotaState": "reserved",
                "http": 1,
                "queued": false
            }
        },
        "transport": { "ordinaryNativeHttp": 1, "proposalHttp": 2, "restartHttp": 1 }
    })
}

fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_string())
        })
        .unwrap_or_default()
}

#[test]
fn pending_golden_fails_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pending.json");
    std::fs::write(
        &path,
        r#"{"schema":"grokptah.shared-black-box-result.v1","calibration":"pending"}"#,
    )
    .unwrap();
    let message = catch_unwind(AssertUnwindSafe(|| {
        load_immutable_golden(&path, "4bd2081b2945e8ce881895f976bb7c8d88b929f2")
    }))
    .expect_err("pending golden must fail");
    let text = panic_text(message);
    assert!(text.contains("pending golden"), "unexpected panic: {text}");
}

#[test]
fn missing_golden_fails_before_launch() {
    let path = PathBuf::from("/tmp/sbb-v1-missing-golden-does-not-exist.json");
    let message = catch_unwind(AssertUnwindSafe(|| {
        load_immutable_golden(&path, "4bd2081b2945e8ce881895f976bb7c8d88b929f2")
    }))
    .expect_err("missing golden must fail");
    let text = panic_text(message);
    assert!(text.contains("missing immutable golden"), "{text}");
}

#[test]
fn malformed_golden_fails_before_launch() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("malformed.json");
    std::fs::write(&path, "{not-json").unwrap();
    let message = catch_unwind(AssertUnwindSafe(|| {
        load_immutable_golden(&path, "4bd2081b2945e8ce881895f976bb7c8d88b929f2")
    }))
    .expect_err("malformed golden must fail");
    let text = panic_text(message);
    assert!(text.contains("malformed golden"), "{text}");
}

#[test]
fn unknown_source_revision_fails_closed() {
    let scenario = Scenario::load();
    let message = catch_unwind(AssertUnwindSafe(|| {
        select_golden_file("ffffffffffffffffffffffffffffffffffffffff", &scenario)
    }))
    .expect_err("unknown revision must fail closed");
    let text = panic_text(message);
    assert!(text.contains("unexpected source revision"), "{text}");
}

#[test]
fn declared_source_revision_rejects_non_sha_forms() {
    for declared in [
        "",
        "94e9437",
        "refs/pull/399/merge",
        "HEAD",
        "94E9437E2C45022FF442092866B5EEA881D2396D",
        "94e9437e2c45022ff442092866b5eea881d2396",
        "94e9437e2c45022ff442092866b5eea881d2396dd",
        "94e9437e2c45022ff442092866b5eea881d2396g",
    ] {
        let message = catch_unwind(AssertUnwindSafe(|| {
            verify_declared_source_revision(declared)
        }))
        .expect_err("a non-sha declaration must fail closed");
        let text = panic_text(message);
        assert!(
            text.contains("is not a full 40-character lowercase commit sha"),
            "{declared}: {text}"
        );
    }
}

#[test]
fn declared_source_revision_rejects_a_commit_absent_from_this_checkout() {
    let message = catch_unwind(AssertUnwindSafe(|| {
        verify_declared_source_revision(&"f".repeat(40))
    }))
    .expect_err("an absent commit must fail closed");
    let text = panic_text(message);
    assert!(text.contains("is not a commit in this checkout"), "{text}");
}

#[test]
fn declared_source_revision_accepts_the_verified_checked_out_head() {
    let head = git_stdout(&["rev-parse", "HEAD"]).expect("git rev-parse HEAD");
    assert_eq!(verify_declared_source_revision(&head), head);
    assert!(host_source_differences(&head, &head).is_empty());
}

#[test]
fn host_source_differences_never_silently_absorbs_a_host_path() {
    for allowed in FIXTURE_ALLOWLIST {
        assert!(allowlisted(allowed), "{allowed}");
    }
    for host_path in [
        "crates/codegen/grokptah-service/src/lib.rs",
        "crates/codegen/grokptah-agent-bridge/src/lib.rs",
        "crates/codegen/grokptah-service/Cargo.lock.other",
        ".github/workflows/hosted-service.yml",
    ] {
        assert!(!allowlisted(host_path), "{host_path}");
    }
}

#[test]
fn an_undeclared_merge_head_is_refused_by_name() {
    assert!(merge_refusal_message("a".repeat(40).as_str(), 1).is_none());
    assert!(merge_refusal_message("a".repeat(40).as_str(), 0).is_none());
    let message = merge_refusal_message("f16f5d7fe66199a1d13326e7f659592ef439d7ee", 2)
        .expect("a merge head must be refused");
    assert!(
        message.contains("merge commit f16f5d7fe66199a1d13326e7f659592ef439d7ee"),
        "{message}"
    );
    assert!(
        message.contains("GROKPTAH_SHARED_BLACK_BOX_SOURCE_REVISION"),
        "{message}"
    );
}

#[test]
fn an_unverified_declaration_never_reaches_golden_selection() {
    // The declaration channel does not widen the audited table: a revision
    // that survives verification is still refused unless it was audited.
    let scenario = Scenario::load();
    let head = git_stdout(&["rev-parse", "HEAD"]).expect("git rev-parse HEAD");
    let verified = verify_declared_source_revision(&head);
    if AUDITED_GOLDENS.iter().any(|(sha, _)| *sha == verified) {
        return;
    }
    let message = catch_unwind(AssertUnwindSafe(|| {
        select_golden_file(&verified, &scenario)
    }))
    .expect_err("a verified but unaudited revision must still fail closed");
    let text = panic_text(message);
    assert!(text.contains("unexpected source revision"), "{text}");
}

#[test]
fn extra_and_missing_semantic_keys_fail() {
    let expected = sample_oracle_result();
    let mut extra = expected.clone();
    extra
        .as_object_mut()
        .unwrap()
        .insert("unexpectedLeaf".into(), json!(1));
    let extra_errors = compare_normalized(&extra, &expected).expect_err("extra key");
    assert!(
        extra_errors
            .iter()
            .any(|row| row.contains("extra semantic key unexpectedLeaf")),
        "{extra_errors:?}"
    );
    let mut missing = expected.clone();
    missing.as_object_mut().unwrap().remove("assertions");
    let missing_errors = compare_normalized(&missing, &expected).expect_err("missing key");
    assert!(
        missing_errors
            .iter()
            .any(|row| row.contains("missing semantic key assertions")),
        "{missing_errors:?}"
    );
}

#[test]
fn altered_cardinality_state_purpose_quota_attempt_retry_fail() {
    let expected = sample_oracle_result();
    let mut actual = expected.clone();
    actual["assertions"]["ordinaryNative"]["work"] = json!(2);
    actual["assertions"]["ordinaryNative"]["quotaState"] = json!("reserved");
    actual["observations"]["native"]["getRun"]["state"] = json!("failed");
    actual["observations"]["native"]["getRun"]["purpose"] = json!("other");
    actual["assertions"]["restart"]["providerAttempt"] = json!(2);
    actual["assertions"]["restart"]["retryClass"] = json!("auto");
    let errors = compare_normalized(&actual, &expected).expect_err("altered semantics");
    let joined = errors.join("\n");
    for needle in [
        "ordinaryNative.work",
        "quotaState",
        "state",
        "purpose",
        "providerAttempt",
        "retryClass",
    ] {
        assert!(joined.contains(needle), "missing {needle} in {joined}");
    }
}

#[test]
fn backend_result_divergence_fails() {
    let desktop = sample_oracle_result();
    let mut hosted = desktop.clone();
    hosted["transport"]["restartHttp"] = json!(2);
    let errors = compare_normalized(&desktop, &hosted).expect_err("divergence");
    assert!(
        errors.iter().any(|row| row.contains("restartHttp")),
        "{errors:?}"
    );
}

#[test]
fn redaction_scan_fails_on_forbidden_secrets() {
    let mut hits = Vec::new();
    scan_raw(
        &json!({
            "providerRoute": {
                "baseUrl": "http://127.0.0.1:9",
                "credentialRef": "ref",
                "credentialFingerprint": "fp"
            },
            "text": "sbb-v1-api-key-1a2b3c4d5e6f708192a3b4c5d6e7f809"
        }),
        "probe",
        RedactionNeedles {
            bearer: "sbb-v1-mcp-bearer-7f3c9e1a4b8d2e6f0c5a9d3b7e1f4a8c",
            api_key: "sbb-v1-api-key-1a2b3c4d5e6f708192a3b4c5d6e7f809",
            gateway: "http://127.0.0.1:9",
            gateway_authority: "127.0.0.1:9",
            marker: "sbb-v1-private-gateway-sentinel",
        },
        &mut hits,
    );
    let reasons: Vec<_> = hits.iter().map(|hit| hit.reason.as_str()).collect();
    assert!(reasons
        .iter()
        .any(|reason| reason.contains("forbidden key")));
    assert!(reasons
        .iter()
        .any(|reason| reason.contains("api-key sentinel")));
}

#[test]
fn update_env_cannot_rewrite_or_bypass() {
    let _lock = home_override_serial();
    let golden = PathBuf::from(GOLDEN_DIR).join("expected-pr352-4bd2081b.json");
    let before = std::fs::read(&golden).expect("read pr golden");
    std::env::set_var("UPDATE_SHARED_BLACK_BOX_GOLDEN", "1");
    let panicked = catch_unwind(reject_golden_mutation_env);
    std::env::remove_var("UPDATE_SHARED_BLACK_BOX_GOLDEN");
    assert!(panicked.is_err(), "update env must fail closed");
    let after = std::fs::read(&golden).expect("re-read pr golden");
    assert_eq!(before, after, "golden bytes must be unchanged");
}

#[test]
fn write_golden_document_roundtrip_does_not_touch_repo_goldens() {
    let result = sample_oracle_result();
    let document = write_golden_document(&result, "4bd2081b2945e8ce881895f976bb7c8d88b929f2");
    assert!(document.get("evidenceHash").is_some());
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oracle.json");
    std::fs::write(&path, serde_json::to_string_pretty(&document).unwrap()).unwrap();
    let loaded = load_immutable_golden(&path, "4bd2081b2945e8ce881895f976bb7c8d88b929f2");
    assert!(loaded.get("evidenceHash").is_none());
    assert_eq!(
        loaded["sourceRevision"],
        json!("4bd2081b2945e8ce881895f976bb7c8d88b929f2")
    );
}

#[test]
fn committed_preload_is_armed() {
    const {
        assert!(
            PRELOAD_IMMUTABLE_GOLDEN,
            "committed fixture must fail closed on missing/pending goldens before launch"
        );
    }
}

#[test]
fn host_capability_oracle_rejects_kind_and_capability_drift() {
    let mut desktop_capabilities = EXPECTED_COMMON_HOST_CAPABILITIES
        .iter()
        .chain(EXPECTED_DESKTOP_LOCAL_HOST_CAPABILITIES.iter())
        .copied()
        .collect::<Vec<_>>();
    desktop_capabilities.sort();
    let document = json!({
        "schema": "grokptah.authority-capabilities.v1",
        "schemaVersion": 1,
        "documentHash": "a".repeat(64),
        "assertedBy": {
            "hostInstanceId": "b".repeat(64),
            "hostKind": "desktop_local",
            "hostVersion": "0.1.0"
        },
        "principal": { "role": "remote_coordinator" },
        "hostCapabilities": desktop_capabilities,
        "hardDenials": ["approval", "promotion", "computer_use"]
    });
    let mut defects = Vec::new();
    host_capability_contract(EndpointKind::Desktop, &document, &mut defects);
    assert!(defects.is_empty(), "{defects:?}");

    let mut drifted = document;
    drifted["assertedBy"]["hostKind"] = json!("standalone_service");
    drifted["hostCapabilities"] = json!(EXPECTED_COMMON_HOST_CAPABILITIES);
    let mut defects = Vec::new();
    host_capability_contract(EndpointKind::Desktop, &drifted, &mut defects);
    let joined = defects.join("\n");
    assert!(joined.contains("assertedBy.hostKind"), "{joined}");
    assert!(joined.contains("hostCapabilities"), "{joined}");
}

#[test]
fn expected_main_golden_is_immutable_for_audited_revision() {
    let path = PathBuf::from(GOLDEN_DIR).join("expected-main-67e29bd3.json");
    let loaded = load_immutable_golden(&path, "67e29bd34dc64049432c715c93c2cef2185c63ea");
    assert_eq!(loaded["overlay"]["completed"], json!(false));
    assert_eq!(
        loaded["overlay"]["mcpError"]["code"],
        json!("invalid_request")
    );
}
