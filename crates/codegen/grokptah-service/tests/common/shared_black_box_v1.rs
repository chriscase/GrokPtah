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
    let source = detect_audited_source_revision();
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

fn git_output_at(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
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
    Ok(output.stdout)
}

/// Text output from Git: object ids, ref names, and configuration values, all
/// of which are ASCII. Paths never travel this way. They are read as bytes,
/// because deciding on a lossy decode would let a byte sequence that merely
/// renders like an allowlisted path pass for one.
fn git_stdout_at(root: &Path, args: &[&str]) -> Result<String, String> {
    let bytes = git_output_at(root, args)?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("git {} produced non-UTF-8 output", args.join(" ")))?;
    Ok(text.trim_end().to_string())
}

/// Paths named by `git status --porcelain=v1 -z`.
///
/// Records are NUL-terminated, so a path is taken verbatim: never split on
/// newlines, never unquoted, never trimmed. Rename and copy entries carry the
/// original path as a second field, and both sides are returned. A rename out
/// of the audited tree into an allowlisted destination changes that tree
/// exactly as much as a plain deletion does, so keeping only the destination
/// would hide it. A field that does not parse as a record is an error, never
/// something to skip.
fn porcelain_paths(status: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut paths = Vec::new();
    let mut fields = status.split(|byte| *byte == 0);
    while let Some(record) = fields.next() {
        if record.is_empty() {
            // The stream ends with the final record's terminator; anything
            // after it means the output was not the shape we asked for.
            if fields.any(|rest| !rest.is_empty()) {
                return Err("trailing data after the final git status record".to_string());
            }
            break;
        }
        if record.len() < 4 || record[2] != b' ' {
            return Err(format!(
                "malformed git status record: {}",
                String::from_utf8_lossy(record)
            ));
        }
        paths.push(record[3..].to_vec());
        if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
            let original = fields
                .next()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    format!(
                        "git status rename record is missing its original path: {}",
                        String::from_utf8_lossy(record)
                    )
                })?;
            paths.push(original.to_vec());
        }
    }
    Ok(paths)
}

/// Paths named by `git diff-tree -z --raw`.
///
/// Each entry is a metadata field beginning with `:`, followed by one path
/// field, or two when the status is a rename or a copy. Both sides are
/// returned, so a build of Git that ignores `--no-renames` still cannot
/// under-report a move.
fn raw_diff_paths(raw: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let mut paths = Vec::new();
    let mut fields = raw.split(|byte| *byte == 0);
    while let Some(field) = fields.next() {
        if field.is_empty() {
            if fields.any(|rest| !rest.is_empty()) {
                return Err("trailing data after the final git diff record".to_string());
            }
            break;
        }
        let metadata = std::str::from_utf8(field)
            .map_err(|_| "git diff metadata field is not valid UTF-8".to_string())?;
        if !metadata.starts_with(':') {
            return Err(format!("malformed git diff metadata field: {metadata}"));
        }
        let status = metadata
            .split_whitespace()
            .next_back()
            .filter(|status| !status.is_empty())
            .ok_or_else(|| format!("git diff metadata field has no status: {metadata}"))?;
        let sides = if status.starts_with(['R', 'C']) { 2 } else { 1 };
        for _ in 0..sides {
            let path = fields
                .next()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| format!("git diff record is missing a path: {metadata}"))?;
            paths.push(path.to_vec());
        }
    }
    Ok(paths)
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

/// Paths a commit changed against its first parent.
///
/// Read from `--raw` records with rename detection off, so a rename or a copy
/// surfaces as a deletion of the source and an addition of the destination
/// rather than as the destination alone. `git diff --name-only` reports only
/// the destination of an exact rename, which lets a commit that moved a file
/// out of the audited tree and into the fixture allowlist read as fixture-only
/// -- and the walk would step straight past a revision whose tree it never
/// built. `--no-renames` also overrides any `diff.renames` the repository
/// itself sets, and `-z` keeps every path verbatim.
///
/// Root-ness is decided by the parent list, never by whether `rev-parse
/// {sha}^` happened to succeed. An absent parent object makes that lookup fail
/// exactly like a real root commit does, and treating it as one would diff the
/// whole tree against nothing -- reporting every path as changed and naming a
/// revision on the strength of a missing object. A parent that is named must
/// therefore also be present.
/// A path is allowlisted only when it is valid UTF-8 and matches the fixture
/// allowlist. A path that is not valid UTF-8 is never allowlisted, so it is
/// treated as a real change rather than decoded into something that resembles
/// a fixture path.
fn allowlisted_path(path: &[u8]) -> bool {
    std::str::from_utf8(path).is_ok_and(allowlisted)
}

fn commit_changed_files_at(root: &Path, sha: &str) -> Result<Vec<Vec<u8>>, String> {
    let parents = parent_shas(root, sha)?;
    let raw = match parents.first() {
        None => git_output_at(
            root,
            &[
                "diff-tree",
                "--no-commit-id",
                "--no-renames",
                "--root",
                "-r",
                "-z",
                "--raw",
                sha,
            ],
        )?,
        Some(first_parent) => {
            require_present_commit(root, first_parent)?;
            git_output_at(
                root,
                &[
                    "diff-tree",
                    "--no-commit-id",
                    "--no-renames",
                    "-r",
                    "-z",
                    "--raw",
                    first_parent,
                    sha,
                ],
            )?
        }
    };
    raw_diff_paths(&raw)
}

/// Resolve the audited host revision, or explain why it cannot be resolved.
///
/// Every failure is terminal. The resolver never falls back to `HEAD`: on a
/// pull-request run `HEAD` is an ephemeral synthetic merge that exists only for
/// that run, so naming it would key an immutable golden to an identity nobody
/// can check out again. Refusing is always correct; guessing never is.
fn resolve_audited_source_revision_at(root: &Path) -> Result<String, String> {
    require_unrewritten_history(root)?;
    require_complete_history(root)?;
    require_clean_worktree(root)?;
    let head = git_stdout_at(root, &["rev-parse", "HEAD"])?;
    let candidate = audited_walk_start(root, &head)?;
    // Validated here, before a single step is taken, so unwrapping the
    // synthetic head can never hand the walk a merge to first-parent through.
    require_present_commit(root, &candidate)?;
    require_ordinary_or_root(root, &candidate)?;
    walk_to_audited_commit(root, &candidate)
}

const REPLACE_REF_BASE_ENV: &str = "GIT_REPLACE_REF_BASE";

/// The one namespace Git reads object replacements from unless the environment
/// moves it. Matched exactly: `refs/replace` without the trailing slash is a
/// different namespace, not a spelling of this one.
const DEFAULT_REPLACE_REF_BASE: &str = "refs/replace/";

/// Replace refs and the legacy graft file both rewrite the parentage that
/// `git rev-list` reports without altering a single commit object, so a
/// resolver that trusts that output can be walked down a forged history and
/// made to name a revision that never had those parents. Their presence is
/// disqualifying on its own. Reading around them with `--no-replace-objects`
/// would hide the tampering instead of surfacing it.
fn require_unrewritten_history(root: &Path) -> Result<(), String> {
    // `GIT_REPLACE_REF_BASE` relocates the namespace Git reads replacements
    // from, so a scan of the default namespace can come back empty while Git
    // is traversing forged parentage out of another one. An inherited
    // relocation is refused outright rather than followed: scanning wherever it
    // points would make the check depend on the very setting an attacker
    // controls. Refusing leaves the default as the only namespace that can be
    // active here, which is the one scanned below.
    if let Some(base) = std::env::var_os(REPLACE_REF_BASE_ENV) {
        if base != OsStr::new(DEFAULT_REPLACE_REF_BASE) {
            return Err(format!(
                "refusing to resolve with a relocated replace namespace: \
                 {REPLACE_REF_BASE_ENV}={} (expected {DEFAULT_REPLACE_REF_BASE}) (fail closed)",
                Path::new(&base).display()
            ));
        }
    }
    let replaced = git_stdout_at(
        root,
        &[
            "for-each-ref",
            "--format=%(refname)",
            DEFAULT_REPLACE_REF_BASE,
        ],
    )?;
    let replaced = replaced.split_whitespace().collect::<Vec<_>>();
    if !replaced.is_empty() {
        return Err(format!(
            "refusing to resolve against rewritten history: {} replace ref(s) present ({}) \
             (fail closed)",
            replaced.len(),
            replaced.join(", ")
        ));
    }
    // Checked as a file rather than by observing its effect, so the refusal
    // holds on Git versions that have already dropped graft support.
    let grafts = git_stdout_at(root, &["rev-parse", "--git-path", "info/grafts"])?;
    let grafts = root.join(grafts.trim());
    if std::fs::metadata(&grafts).is_ok_and(|meta| meta.len() > 0) {
        return Err(format!(
            "refusing to resolve against rewritten history: legacy graft file {} (fail closed)",
            grafts.display()
        ));
    }
    Ok(())
}

/// Refuse to resolve inside a shallow clone. A truncated history stops the
/// audited walk at whatever commit happens to be the oldest object present,
/// which silently names the wrong revision.
fn require_complete_history(root: &Path) -> Result<(), String> {
    let shallow = git_stdout_at(root, &["rev-parse", "--is-shallow-repository"])?;
    if shallow.trim() != "false" {
        return Err(
            "shallow checkout cannot identify the audited source revision; \
             check out full history (fail closed)"
                .to_string(),
        );
    }
    Ok(())
}

fn require_clean_worktree(root: &Path) -> Result<(), String> {
    let status = git_output_at(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    for path in porcelain_paths(&status)? {
        if !allowlisted_path(&path) {
            return Err(format!(
                "unexpected dirty path outside fixture allowlist: {}",
                String::from_utf8_lossy(&path)
            ));
        }
    }
    Ok(())
}

/// Parents of `sha`, with the identity `rev-list` echoed back checked against
/// the commit we asked about.
fn parent_shas(root: &Path, sha: &str) -> Result<Vec<String>, String> {
    // Reading parents requires traversing to them, so this is also where an
    // absent parent object surfaces. Name that plainly instead of leaking a
    // raw Git failure, and never treat it as "no parents".
    let line =
        git_stdout_at(root, &["rev-list", "--parents", "-n", "1", sha]).map_err(|error| {
            format!(
                "cannot read the parents of {sha}; this checkout's object store is incomplete \
             (fail closed): {error}"
            )
        })?;
    let mut fields = line.split_whitespace();
    let listed = fields.next().unwrap_or_default();
    if listed != sha {
        return Err(format!(
            "rev-list named {listed} for {sha}; refusing to resolve an ambiguous commit"
        ));
    }
    Ok(fields.map(str::to_string).collect())
}

/// Pick the commit the audited walk starts from.
///
/// Only two shapes are accepted at the checked-out head:
/// * an ordinary (or root) commit, which is its own candidate; and
/// * a two-parent merge whose tree is identical to exactly its second parent,
///   which is how a hosted pull-request checkout wraps the revision under test.
///
/// An ordinary commit is never unwrapped to its parent: that would name a
/// revision the runner never checked out.
fn audited_walk_start(root: &Path, head: &str) -> Result<String, String> {
    let parents = parent_shas(root, head)?;
    match parents.len() {
        0 | 1 => Ok(head.to_string()),
        2 => matching_tree_parent(root, head, &parents[0], &parents[1]),
        count => Err(format!(
            "HEAD {head} is an octopus merge with {count} parents; only a unique two-parent \
             matching-tree merge can be resolved (fail closed)"
        )),
    }
}

/// A hosted pull-request checkout is a synthetic merge of the base branch into
/// the revision under test, so its tree is byte-identical to that revision's
/// tree and the revision can be recovered exactly. That recovery is only sound
/// when the second parent is the *only* tree that matches: if the base side
/// matches too the merge is empty and either parent would fit, and if neither
/// matches the merge resolved real content that exists in no single commit.
/// Both are unrecoverable, and both fail closed.
fn matching_tree_parent(
    root: &Path,
    head: &str,
    first_parent: &str,
    second_parent: &str,
) -> Result<String, String> {
    require_present_commit(root, first_parent)?;
    require_present_commit(root, second_parent)?;
    let head_tree = tree_of(root, head)?;
    let first_matches = tree_of(root, first_parent)? == head_tree;
    let second_matches = tree_of(root, second_parent)? == head_tree;
    match (first_matches, second_matches) {
        (false, true) => Ok(second_parent.to_string()),
        (true, true) => Err(format!(
            "merge {head} shares its tree with both parents ({first_parent}, {second_parent}); \
             refusing to guess the audited source (fail closed)"
        )),
        (true, false) => Err(format!(
            "merge {head} shares its tree only with its base parent {first_parent}; the audited \
             revision contributed nothing and cannot be named (fail closed)"
        )),
        (false, false) => Err(format!(
            "merge {head} shares its tree with neither parent; the audited source is not \
             recoverable (fail closed)"
        )),
    }
}

/// A commit named in the graph but absent from the object store is the
/// signature of a truncated clone. Naming a revision from it would be a guess.
fn require_present_commit(root: &Path, sha: &str) -> Result<(), String> {
    git_stdout_at(root, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
        .map(|_| ())
        .map_err(|_| format!("commit {sha} is missing from this checkout (fail closed)"))
}

/// Commits the audited walk stands on must have at most one parent. Unwrapping
/// the synthetic head can land on a merge, and walking one from its first
/// parent would silently ignore everything the other side contributed --
/// naming a revision whose tree the audited run never built.
fn require_ordinary_or_root(root: &Path, sha: &str) -> Result<(), String> {
    require_ordinary_topology(sha, &parent_shas(root, sha)?)
}

fn require_ordinary_topology(sha: &str, parents: &[String]) -> Result<(), String> {
    if parents.len() > 1 {
        return Err(format!(
            "audited candidate {sha} has {} parents; an audited revision must be an ordinary \
             commit, not a merge (fail closed)",
            parents.len()
        ));
    }
    Ok(())
}

fn tree_of(root: &Path, sha: &str) -> Result<String, String> {
    git_stdout_at(root, &["rev-parse", &format!("{sha}^{{tree}}")])
}

/// Walk first-parent history from the candidate until a commit changes
/// something outside the fixture allowlist. That commit is the audited host
/// revision: the newest revision whose behaviour a golden may describe. Every
/// commit stood on is checked for presence and for ordinary topology first, so
/// the walk can never traverse a merge or a missing object. Running off the end
/// of history means no such commit exists here, which is a failure, not a
/// reason to fall back to the head.
fn walk_to_audited_commit(root: &Path, candidate: &str) -> Result<String, String> {
    let mut sha = candidate.to_string();
    loop {
        require_present_commit(root, &sha)?;
        let parents = parent_shas(root, &sha)?;
        require_ordinary_topology(&sha, &parents)?;
        let files = commit_changed_files_at(root, &sha)?;
        if files.iter().any(|path| !allowlisted_path(path)) {
            return Ok(sha);
        }
        match parents.first() {
            Some(parent) => sha = parent.clone(),
            None => {
                return Err("could not identify audited host revision (fail closed)".to_string())
            }
        }
    }
}

fn detect_audited_source_revision_at(root: &Path) -> String {
    resolve_audited_source_revision_at(root)
        .unwrap_or_else(|error| panic!("audited source revision: {error}"))
}

fn detect_audited_source_revision() -> String {
    detect_audited_source_revision_at(&repo_root())
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

// --- Audited-source resolver: adversarial topology coverage -----------------
//
// Each test builds a real repository whose shape the resolver must either name
// exactly or refuse outright. The refusals matter as much as the successes: a
// resolver that fell back to HEAD would key an immutable golden to an ephemeral
// synthetic merge, and one that trusted rewritten parentage or first-parent
// walked a merge would name a revision the audited run never built.

fn detector_git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run detector git command");
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn detector_write(repo: &Path, path: &str, contents: &str) {
    let path = repo.join(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("detector fixture parent");
    }
    std::fs::write(path, contents).expect("detector fixture file");
}

fn detector_commit(repo: &Path, message: &str) -> String {
    detector_git(repo, &["add", "--all"]);
    detector_git(repo, &["commit", "--quiet", "-m", message]);
    detector_git(repo, &["rev-parse", "HEAD"])
}

/// A repository whose only commit touches an allowlisted fixture path, so the
/// audited walk must keep walking past it.
fn detector_repo() -> TempDir {
    let repo = tempfile::tempdir().expect("detector repo");
    detector_git(repo.path(), &["init", "--quiet", "-b", "main"]);
    detector_git(
        repo.path(),
        &["config", "user.email", "detector-tests@example.invalid"],
    );
    detector_git(repo.path(), &["config", "user.name", "detector tests"]);
    detector_git(repo.path(), &["config", "commit.gpgsign", "false"]);
    detector_git(
        repo.path(),
        &["config", "advice.graftFileDeprecated", "false"],
    );
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/shared_black_box_v1.rs",
        "fixture-only base\n",
    );
    detector_commit(repo.path(), "allowlisted base");
    repo
}

/// Branch `name` off the current head with one commit outside the allowlist.
fn detector_side_branch(repo: &Path, name: &str) -> String {
    detector_git(repo, &["checkout", "--quiet", "-b", name]);
    detector_write(repo, &format!("{name}-only.txt"), name);
    detector_commit(repo, name)
}

/// `main` merged with a branch it is already up to date with: the merge tree
/// equals the second parent's tree exactly, which is the hosted pull-request
/// shape.
fn matching_tree_repo() -> (TempDir, String, String) {
    let repo = detector_repo();
    let candidate = detector_side_branch(repo.path(), "candidate");
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_git(
        repo.path(),
        &["merge", "--quiet", "--no-ff", "--no-edit", "candidate"],
    );
    let merge = detector_git(repo.path(), &["rev-parse", "HEAD"]);
    (repo, candidate, merge)
}

#[test]
fn direct_candidate_resolves_to_the_checked_out_commit() {
    let repo = detector_repo();
    detector_write(repo.path(), "direct-only.txt", "direct\n");
    let head = detector_commit(repo.path(), "ordinary commit outside fixture allowlist");
    assert_eq!(
        detector_git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        2,
        "ordinary commit"
    );
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("direct candidate"),
        head
    );
}

#[test]
fn direct_candidate_walks_past_allowlisted_only_commits() {
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    let audited = detector_commit(repo.path(), "audited change");
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/fixtures/shared-black-box/v1/scenario.json",
        "{}\n",
    );
    let head = detector_commit(repo.path(), "allowlisted fixture-only change");
    assert_ne!(head, audited);
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("walk past fixture commits"),
        audited,
        "a fixture-only commit must not become the audited revision"
    );
}

#[test]
fn matching_tree_two_parent_merge_resolves_to_the_second_parent() {
    let (repo, candidate, merge) = matching_tree_repo();
    assert_ne!(candidate, merge);
    let resolved = resolve_audited_source_revision_at(repo.path()).expect("matching-tree merge");
    assert_eq!(resolved, candidate);
    assert_ne!(
        resolved, merge,
        "the ephemeral synthetic merge must never be named"
    );
}

#[test]
fn merge_whose_tree_matches_neither_parent_fails_closed() {
    let repo = detector_repo();
    detector_side_branch(repo.path(), "candidate");
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_write(repo.path(), "base-only.txt", "base branch\n");
    detector_commit(repo.path(), "base branch change");
    detector_git(
        repo.path(),
        &["merge", "--quiet", "--no-ff", "--no-edit", "candidate"],
    );
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a real merge is not a recoverable audited source");
    assert!(error.contains("neither parent"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn merge_whose_tree_matches_both_parents_is_ambiguous_and_fails_closed() {
    let repo = detector_repo();
    detector_write(repo.path(), "shared.txt", "shared\n");
    detector_commit(repo.path(), "shared content outside fixture allowlist");
    detector_git(repo.path(), &["checkout", "--quiet", "-b", "candidate"]);
    // Change a file and change it straight back: a distinct commit whose tree
    // is identical to the base branch's tree.
    detector_write(repo.path(), "shared.txt", "diverged\n");
    detector_commit(repo.path(), "diverge");
    detector_write(repo.path(), "shared.txt", "shared\n");
    detector_commit(repo.path(), "converge back onto the base tree");
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_git(
        repo.path(),
        &["merge", "--quiet", "--no-ff", "--no-edit", "candidate"],
    );
    let head = detector_git(repo.path(), &["rev-parse", "HEAD"]);
    let head_tree = detector_git(repo.path(), &["rev-parse", &format!("{head}^{{tree}}")]);
    let parents = detector_git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    let parents = parents.split_whitespace().skip(1).collect::<Vec<_>>();
    assert_eq!(parents.len(), 2);
    for parent in &parents {
        assert_eq!(
            detector_git(repo.path(), &["rev-parse", &format!("{parent}^{{tree}}")]),
            head_tree,
            "both parents must share the merge tree for this case"
        );
    }
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("an ambiguous merge tree must not resolve");
    assert!(error.contains("both parents"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn merge_whose_tree_matches_only_the_base_parent_fails_closed() {
    // A merge that discards the candidate side entirely: two real parents, but
    // the merge tree is the base tree. Naming the base here would attribute the
    // run to a revision that contributed nothing under test.
    let repo = detector_repo();
    detector_write(repo.path(), "base-only.txt", "base\n");
    detector_commit(repo.path(), "base outside fixture allowlist");
    let candidate = detector_side_branch(repo.path(), "candidate");
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_write(repo.path(), "base-advance.txt", "advance\n");
    let base = detector_commit(repo.path(), "base advances past the candidate");
    detector_git(
        repo.path(),
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "--no-edit",
            "-s",
            "ours",
            "candidate",
        ],
    );
    let head = detector_git(repo.path(), &["rev-parse", "HEAD"]);
    let parents = detector_git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    let parents = parents.split_whitespace().skip(1).collect::<Vec<_>>();
    assert_eq!(parents, vec![base.as_str(), candidate.as_str()]);
    let head_tree = detector_git(repo.path(), &["rev-parse", &format!("{head}^{{tree}}")]);
    assert_eq!(
        head_tree,
        detector_git(repo.path(), &["rev-parse", &format!("{base}^{{tree}}")]),
        "the merge must carry the base tree"
    );
    assert_ne!(
        head_tree,
        detector_git(
            repo.path(),
            &["rev-parse", &format!("{candidate}^{{tree}}")]
        ),
        "the merge must not carry the candidate tree"
    );
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("an empty candidate must not resolve to the base parent");
    assert!(error.contains("base parent"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn octopus_merge_fails_closed() {
    let repo = detector_repo();
    for name in ["one", "two", "three"] {
        detector_git(repo.path(), &["checkout", "--quiet", "main"]);
        detector_side_branch(repo.path(), name);
    }
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_git(
        repo.path(),
        &[
            "merge",
            "--quiet",
            "--no-ff",
            "--no-edit",
            "one",
            "two",
            "three",
        ],
    );
    assert_eq!(
        detector_git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        5,
        "octopus with three sides"
    );
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("an octopus merge must not resolve");
    assert!(error.contains("octopus merge"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

/// Wrap `branch` in a synthetic two-parent merge on `main` whose tree matches
/// the branch tip exactly, mirroring a hosted pull-request checkout.
fn synthetic_pull_request_merge(repo: &Path, branch: &str) -> String {
    detector_git(repo, &["checkout", "--quiet", "main"]);
    detector_git(repo, &["merge", "--quiet", "--no-ff", "--no-edit", branch]);
    let merge = detector_git(repo, &["rev-parse", "HEAD"]);
    let merge_tree = detector_git(repo, &["rev-parse", &format!("{merge}^{{tree}}")]);
    let branch_tree = detector_git(repo, &["rev-parse", &format!("{branch}^{{tree}}")]);
    assert_eq!(
        merge_tree, branch_tree,
        "synthetic merge must match the tip"
    );
    merge
}

#[test]
fn candidate_merge_below_the_synthetic_head_fails_closed() {
    // The head unwraps cleanly, but the revision it unwraps to is itself a
    // merge. First-parent walking it would hide the second side entirely.
    let repo = detector_repo();
    detector_side_branch(repo.path(), "feature");
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_side_branch(repo.path(), "candidate");
    detector_git(
        repo.path(),
        &["merge", "--quiet", "--no-ff", "--no-edit", "feature"],
    );
    let candidate = detector_git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        detector_git(
            repo.path(),
            &["rev-list", "--parents", "-n", "1", &candidate]
        )
        .split_whitespace()
        .count(),
        3,
        "candidate must itself be a two-parent merge"
    );
    let merge = synthetic_pull_request_merge(repo.path(), "candidate");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a merge candidate must not be first-parent walked");
    assert!(error.contains(&candidate), "{error}");
    assert!(error.contains("has 2 parents"), "{error}");
    assert!(error.contains("not a merge"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
    assert!(!error.contains(&merge), "must not name the synthetic merge");
}

#[test]
fn candidate_octopus_below_the_synthetic_head_fails_closed() {
    let repo = detector_repo();
    for name in ["alpha", "beta"] {
        detector_git(repo.path(), &["checkout", "--quiet", "main"]);
        detector_side_branch(repo.path(), name);
    }
    detector_git(repo.path(), &["checkout", "--quiet", "main"]);
    detector_side_branch(repo.path(), "candidate");
    detector_git(
        repo.path(),
        &["merge", "--quiet", "--no-ff", "--no-edit", "alpha", "beta"],
    );
    let candidate = detector_git(repo.path(), &["rev-parse", "HEAD"]);
    assert_eq!(
        detector_git(
            repo.path(),
            &["rev-list", "--parents", "-n", "1", &candidate]
        )
        .split_whitespace()
        .count(),
        4,
        "candidate must itself be an octopus merge"
    );
    let merge = synthetic_pull_request_merge(repo.path(), "candidate");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("an octopus candidate must not be first-parent walked");
    assert!(error.contains(&candidate), "{error}");
    assert!(error.contains("has 3 parents"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
    assert!(!error.contains(&merge), "must not name the synthetic merge");
}

#[test]
fn replace_ref_fails_closed() {
    // A replace ref rewrites the parentage `rev-list` reports without touching
    // a single commit object, so the walk would follow a forged history.
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    detector_commit(repo.path(), "audited change");
    detector_write(repo.path(), "later.txt", "later\n");
    let head = detector_commit(repo.path(), "later change");
    assert!(resolve_audited_source_revision_at(repo.path()).is_ok());

    let root = detector_git(repo.path(), &["rev-list", "--max-parents=0", "HEAD"]);
    detector_git(repo.path(), &["replace", "--graft", &head, &root]);
    assert!(
        !detector_git(
            repo.path(),
            &["for-each-ref", "--format=%(refname)", "refs/replace/"]
        )
        .is_empty(),
        "the replace ref must exist for this case"
    );
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("replace refs must not be resolved against");
    assert!(error.contains("rewritten history"), "{error}");
    assert!(error.contains("replace ref"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn legacy_graft_file_fails_closed() {
    // Checked as a file, so the refusal holds whether or not this Git still
    // honours grafts.
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    detector_commit(repo.path(), "audited change");
    detector_write(repo.path(), "later.txt", "later\n");
    let head = detector_commit(repo.path(), "later change");
    assert!(resolve_audited_source_revision_at(repo.path()).is_ok());

    let root = detector_git(repo.path(), &["rev-list", "--max-parents=0", "HEAD"]);
    let grafts = repo.path().join(".git/info/grafts");
    std::fs::create_dir_all(grafts.parent().expect("grafts parent")).expect("grafts dir");
    std::fs::write(&grafts, format!("{head} {root}\n")).expect("write graft file");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a graft file must not be resolved against");
    assert!(error.contains("rewritten history"), "{error}");
    assert!(error.contains("graft file"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn shallow_history_fails_closed_instead_of_naming_the_merge() {
    let (repo, candidate, merge) = matching_tree_repo();
    let parent = tempfile::tempdir().expect("shallow clone parent");
    let clone = parent.path().join("clone");
    let output = Command::new("git")
        .args(["clone", "--quiet", "--depth", "1", "--branch", "main"])
        .arg(format!("file://{}", repo.path().display()))
        .arg(&clone)
        .output()
        .expect("clone shallow detector repo");
    assert!(
        output.status.success(),
        "git clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(detector_git(&clone, &["rev-parse", "HEAD"]), merge);
    let error = resolve_audited_source_revision_at(&clone)
        .expect_err("a shallow checkout must not resolve an audited revision");
    assert!(error.contains("shallow checkout"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
    assert!(!error.contains(&merge), "{error}");
    assert!(!error.contains(&candidate), "{error}");
}

#[test]
fn missing_commit_object_fails_closed() {
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    let audited = detector_commit(repo.path(), "audited change");
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "// fixture-only\n",
    );
    detector_commit(repo.path(), "allowlisted fixture-only change");
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("resolves while intact"),
        audited
    );

    // Drop the audited commit's loose object: the graph still names it, but the
    // object store no longer holds it.
    let object = repo
        .path()
        .join(".git/objects")
        .join(&audited[..2])
        .join(&audited[2..]);
    std::fs::remove_file(&object).expect("remove loose commit object");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a missing commit object must fail closed");
    assert!(error.contains("object store is incomplete"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
    assert!(
        !error.contains("could not identify audited host revision"),
        "an absent object must not be reported as an exhausted history: {error}"
    );
}

#[test]
fn history_without_an_audited_commit_fails_closed() {
    // Every commit touches only allowlisted fixture paths, so the walk runs off
    // the end of history. That is a failure, not a fallback to HEAD.
    let repo = detector_repo();
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "// fixture-only\n",
    );
    detector_commit(repo.path(), "another allowlisted change");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("history with no audited commit must fail closed");
    assert!(
        error.contains("could not identify audited host revision"),
        "{error}"
    );
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn dirty_path_outside_the_fixture_allowlist_fails_closed() {
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    detector_commit(repo.path(), "audited change");
    detector_write(repo.path(), "uncommitted-source.rs", "fn main() {}\n");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a dirty non-fixture path must fail closed");
    assert!(error.contains("unexpected dirty path"), "{error}");
}

// --- Rename, copy, and replace-namespace evasion ---------------------------
//
// Each shape below changes a tree outside the fixture allowlist while trying to
// read as a fixture-only change, or moves the forged-parentage machinery out of
// the namespace being scanned. Tree ids are asserted directly so a test cannot
// pass by exercising a commit that changed nothing.

/// `git mv`, with the destination directory created first: Git will not create
/// it, and a failed move would silently turn these cases into no-ops.
fn detector_git_mv(repo: &Path, from: &str, to: &str) {
    if let Some(parent) = repo.join(to).parent() {
        std::fs::create_dir_all(parent).expect("rename destination directory");
    }
    detector_git(repo, &["mv", from, to]);
}

fn detector_tree(repo: &Path, rev: &str) -> String {
    detector_git(repo, &["rev-parse", &format!("{rev}^{{tree}}")])
}

/// Assert the commit really did change the tree, so "the resolver stopped
/// here" means it caught a change rather than agreeing with a no-op.
fn assert_tree_changed(repo: &Path, sha: &str) {
    assert_ne!(
        detector_tree(repo, &format!("{sha}^")),
        detector_tree(repo, sha),
        "parent and head trees must differ for this case to mean anything"
    );
}

/// A repo whose base commit holds one file outside the allowlist and one
/// inside it, both with enough content for exact-rename detection to fire.
fn rename_detector_repo() -> TempDir {
    let repo = detector_repo();
    detector_write(
        repo.path(),
        "outside-resolver-source.rs",
        "outside resolver source\nline two\nline three\nline four\n",
    );
    detector_commit(repo.path(), "outside source outside the fixture allowlist");
    repo
}

#[test]
fn committed_rename_out_of_the_audited_tree_is_not_fixture_only() {
    // The exact shape the audit reproduced: R100 from a non-fixture path into
    // an allowlisted destination. `git diff --name-only` names only the
    // destination, which would let the walk step straight past this commit.
    let repo = rename_detector_repo();
    detector_git_mv(
        repo.path(),
        "outside-resolver-source.rs",
        "crates/codegen/grokptah-service/tests/common/mod.rs",
    );
    let renamed = detector_commit(repo.path(), "rename outside source into the allowlist");
    assert_tree_changed(repo.path(), &renamed);

    let raw = detector_git(repo.path(), &["diff", "--name-only", "HEAD^", "HEAD"]);
    assert!(
        !raw.contains("outside-resolver-source.rs"),
        "rename detection must actually be hiding the source for this case: {raw}"
    );

    let changed = commit_changed_files_at(repo.path(), &renamed).expect("changed files");
    let changed = changed
        .iter()
        .map(|path| String::from_utf8(path.clone()).expect("utf8 path"))
        .collect::<Vec<_>>();
    assert!(
        changed
            .iter()
            .any(|path| path == "outside-resolver-source.rs"),
        "the deleted source must surface: {changed:?}"
    );
    assert!(
        changed
            .iter()
            .any(|path| path == "crates/codegen/grokptah-service/tests/common/mod.rs"),
        "the added destination must surface: {changed:?}"
    );
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("resolves"),
        renamed,
        "a rename out of the audited tree is not a fixture-only change"
    );
}

#[test]
fn committed_rename_into_the_audited_tree_is_not_fixture_only() {
    let repo = detector_repo();
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "fixture module\nline two\nline three\nline four\n",
    );
    detector_commit(repo.path(), "allowlisted fixture module");
    detector_git_mv(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "outside-resolver-destination.rs",
    );
    let renamed = detector_commit(repo.path(), "rename allowlisted file out of the allowlist");
    assert_tree_changed(repo.path(), &renamed);

    let changed = commit_changed_files_at(repo.path(), &renamed).expect("changed files");
    let changed = changed
        .iter()
        .map(|path| String::from_utf8(path.clone()).expect("utf8 path"))
        .collect::<Vec<_>>();
    assert!(
        changed
            .iter()
            .any(|path| path == "crates/codegen/grokptah-service/tests/common/mod.rs"),
        "the deleted allowlisted source must surface: {changed:?}"
    );
    assert!(
        changed
            .iter()
            .any(|path| path == "outside-resolver-destination.rs"),
        "the added outside destination must surface: {changed:?}"
    );
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("resolves"),
        renamed
    );
}

#[test]
fn repository_rename_and_copy_configuration_cannot_hide_a_move() {
    // `--no-renames` must win over configuration the repository itself sets,
    // including copy detection, which folds an added path into its source.
    let repo = rename_detector_repo();
    detector_git(repo.path(), &["config", "diff.renames", "copies"]);
    detector_git(repo.path(), &["config", "diff.renameLimit", "10000"]);
    detector_git(repo.path(), &["config", "status.renames", "copies"]);
    detector_git_mv(
        repo.path(),
        "outside-resolver-source.rs",
        "crates/codegen/grokptah-service/tests/common/mod.rs",
    );
    let renamed = detector_commit(repo.path(), "rename under copy-detecting configuration");
    assert_tree_changed(repo.path(), &renamed);
    let changed = commit_changed_files_at(repo.path(), &renamed).expect("changed files");
    assert!(
        changed
            .iter()
            .any(|path| path.as_slice() == b"outside-resolver-source.rs"),
        "repository configuration must not suppress the source side"
    );
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("resolves"),
        renamed
    );
}

#[test]
fn committed_copy_into_the_audited_tree_surfaces_the_destination() {
    // A copy leaves its source in place, so only the destination changes the
    // tree. The destination is outside the allowlist, and must be reported.
    let repo = detector_repo();
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "fixture module\nline two\nline three\nline four\n",
    );
    detector_commit(repo.path(), "allowlisted fixture module");
    detector_git(repo.path(), &["config", "diff.renames", "copies"]);
    detector_write(
        repo.path(),
        "outside-resolver-copy.rs",
        "fixture module\nline two\nline three\nline four\n",
    );
    let copied = detector_commit(
        repo.path(),
        "copy allowlisted content outside the allowlist",
    );
    assert_tree_changed(repo.path(), &copied);
    let changed = commit_changed_files_at(repo.path(), &copied).expect("changed files");
    assert!(
        changed
            .iter()
            .any(|path| path.as_slice() == b"outside-resolver-copy.rs"),
        "the copy destination must surface"
    );
    assert_eq!(
        resolve_audited_source_revision_at(repo.path()).expect("resolves"),
        copied
    );
}

#[test]
fn staged_rename_into_the_allowlist_fails_closed() {
    let repo = rename_detector_repo();
    detector_git_mv(
        repo.path(),
        "outside-resolver-source.rs",
        "crates/codegen/grokptah-service/tests/common/mod.rs",
    );
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a staged rename out of the audited tree must fail closed");
    assert!(error.contains("unexpected dirty path"), "{error}");
    assert!(
        error.contains("outside-resolver-source.rs"),
        "the dropped source must be the path named: {error}"
    );
}

#[test]
fn unstaged_rename_into_the_allowlist_fails_closed() {
    let repo = rename_detector_repo();
    std::fs::create_dir_all(
        repo.path()
            .join("crates/codegen/grokptah-service/tests/common"),
    )
    .expect("destination directory");
    std::fs::rename(
        repo.path().join("outside-resolver-source.rs"),
        repo.path()
            .join("crates/codegen/grokptah-service/tests/common/mod.rs"),
    )
    .expect("worktree rename");
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("an unstaged rename out of the audited tree must fail closed");
    assert!(error.contains("unexpected dirty path"), "{error}");
    assert!(
        error.contains("outside-resolver-source.rs"),
        "the dropped source must be the path named: {error}"
    );
}

#[test]
fn staged_copy_outside_the_allowlist_fails_closed() {
    let repo = detector_repo();
    detector_write(
        repo.path(),
        "crates/codegen/grokptah-service/tests/common/mod.rs",
        "fixture module\nline two\nline three\nline four\n",
    );
    detector_commit(repo.path(), "allowlisted fixture module");
    detector_git(repo.path(), &["config", "status.renames", "copies"]);
    detector_write(
        repo.path(),
        "outside-resolver-copy.rs",
        "fixture module\nline two\nline three\nline four\n",
    );
    detector_git(repo.path(), &["add", "--all"]);
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a staged copy outside the allowlist must fail closed");
    assert!(error.contains("unexpected dirty path"), "{error}");
    assert!(error.contains("outside-resolver-copy.rs"), "{error}");
}

#[test]
fn alternate_replace_ref_base_fails_closed() {
    // Forged parentage parked outside `refs/replace/`: the default-namespace
    // scan comes back empty while Git traverses the forgery.
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    detector_commit(repo.path(), "audited change");
    detector_write(repo.path(), "later.txt", "later\n");
    let head = detector_commit(repo.path(), "later change");
    assert!(resolve_audited_source_revision_at(repo.path()).is_ok());
    let root = detector_git(repo.path(), &["rev-list", "--max-parents=0", "HEAD"]);
    let real_parent = detector_git(repo.path(), &["rev-parse", "HEAD^"]);

    let mut env = ProcessEnvGuard::new();
    env.set(REPLACE_REF_BASE_ENV, "refs/audit-escape/");
    detector_git(repo.path(), &["replace", "--graft", &head, &root]);
    assert!(
        detector_git(
            repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname)",
                DEFAULT_REPLACE_REF_BASE
            ]
        )
        .is_empty(),
        "the default namespace must look clean for this case"
    );
    assert!(
        !detector_git(
            repo.path(),
            &["for-each-ref", "--format=%(refname)", "refs/audit-escape/"]
        )
        .is_empty(),
        "the forgery must live in the alternate namespace"
    );
    let forged = detector_git(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert!(
        forged.contains(&root) && !forged.contains(&real_parent),
        "Git must actually be traversing the forged parentage: {forged}"
    );

    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a relocated replace namespace must fail closed");
    assert!(error.contains("relocated replace namespace"), "{error}");
    assert!(error.contains(REPLACE_REF_BASE_ENV), "{error}");
    assert!(error.contains("refs/audit-escape/"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
    // Never neutralized: the forgery is still there, and still refused.
    assert!(!detector_git(
        repo.path(),
        &["for-each-ref", "--format=%(refname)", "refs/audit-escape/"]
    )
    .is_empty());
}

#[test]
fn explicit_default_replace_ref_base_is_inspected() {
    let repo = detector_repo();
    detector_write(repo.path(), "audited-only.txt", "audited\n");
    detector_commit(repo.path(), "audited change");
    detector_write(repo.path(), "later.txt", "later\n");
    let head = detector_commit(repo.path(), "later change");
    let root = detector_git(repo.path(), &["rev-list", "--max-parents=0", "HEAD"]);

    let mut env = ProcessEnvGuard::new();
    env.set(REPLACE_REF_BASE_ENV, DEFAULT_REPLACE_REF_BASE);
    // Spelled out explicitly, the default namespace is accepted and resolution
    // proceeds exactly as it does with the variable unset.
    assert!(resolve_audited_source_revision_at(repo.path()).is_ok());

    // ...and it is genuinely inspected, not merely waved through.
    detector_git(repo.path(), &["replace", "--graft", &head, &root]);
    let error = resolve_audited_source_revision_at(repo.path())
        .expect_err("a replace ref in the explicit default namespace must be caught");
    assert!(error.contains("rewritten history"), "{error}");
    assert!(error.contains("replace ref"), "{error}");
    assert!(error.contains("fail closed"), "{error}");
}

#[test]
fn replace_ref_base_environment_is_restored_after_each_guard() {
    let before = std::env::var_os(REPLACE_REF_BASE_ENV);
    {
        let mut env = ProcessEnvGuard::new();
        env.set(REPLACE_REF_BASE_ENV, "refs/audit-escape/");
        assert_eq!(
            std::env::var_os(REPLACE_REF_BASE_ENV).as_deref(),
            Some(OsStr::new("refs/audit-escape/"))
        );
    }
    assert_eq!(
        std::env::var_os(REPLACE_REF_BASE_ENV),
        before,
        "the guard must restore the environment for the next test"
    );
}

// --- Structural parsing ----------------------------------------------------

#[test]
fn porcelain_records_are_parsed_structurally() {
    let paths = |records: &[u8]| {
        porcelain_paths(records).map(|paths| {
            paths
                .iter()
                .map(|path| String::from_utf8_lossy(path).into_owned())
                .collect::<Vec<_>>()
        })
    };

    assert_eq!(paths(b"").expect("empty status"), Vec::<String>::new());
    assert_eq!(
        paths(b" M src/lib.rs\0?? other.rs\0").expect("plain records"),
        vec!["src/lib.rs", "other.rs"]
    );
    // A rename record yields both sides, destination first.
    assert_eq!(
        paths(b"R  crates/codegen/grokptah-service/tests/common/mod.rs\0outside.rs\0")
            .expect("rename record"),
        vec![
            "crates/codegen/grokptah-service/tests/common/mod.rs",
            "outside.rs"
        ]
    );
    assert_eq!(
        paths(b"C  dest.rs\0source.rs\0").expect("copy record"),
        vec!["dest.rs", "source.rs"]
    );
    // A newline inside a path is data, not a record boundary.
    assert_eq!(
        paths(b"?? weird\nname.rs\0").expect("embedded newline"),
        vec!["weird\nname.rs"]
    );

    for malformed in [
        &b"R  dest.rs\0"[..],      // rename missing its original path
        &b"R  dest.rs\0\0"[..],    // rename with an empty original path
        &b"XY\0"[..],              // no separator and no path
        &b" M\0"[..],              // status with no path
        &b"M src/lib.rs\0"[..],    // one status character, so no space at index 2
        &b"\0 M src/lib.rs\0"[..], // data after an empty leading record
    ] {
        assert!(
            porcelain_paths(malformed).is_err(),
            "malformed status record must be an error, not skipped: {:?}",
            String::from_utf8_lossy(malformed)
        );
    }
}

#[test]
fn raw_diff_records_are_parsed_structurally() {
    let paths = |records: &[u8]| {
        raw_diff_paths(records).map(|paths| {
            paths
                .iter()
                .map(|path| String::from_utf8_lossy(path).into_owned())
                .collect::<Vec<_>>()
        })
    };

    assert_eq!(paths(b"").expect("empty diff"), Vec::<String>::new());
    assert_eq!(
        paths(b":100644 100644 aaaaaaa bbbbbbb M\0src/lib.rs\0").expect("modify record"),
        vec!["src/lib.rs"]
    );
    assert_eq!(
        paths(b":000000 100644 0000000 aaaaaaa A\0added.rs\0:100644 000000 aaaaaaa 0000000 D\0removed.rs\0")
            .expect("add and delete"),
        vec!["added.rs", "removed.rs"]
    );
    // Both sides, even if a Git build ignores --no-renames.
    assert_eq!(
        paths(b":100644 100644 aaaaaaa aaaaaaa R100\0source.rs\0dest.rs\0").expect("rename record"),
        vec!["source.rs", "dest.rs"]
    );
    assert_eq!(
        paths(b":100644 100644 aaaaaaa aaaaaaa C100\0source.rs\0copy.rs\0").expect("copy record"),
        vec!["source.rs", "copy.rs"]
    );

    for malformed in [
        &b":100644 100644 aaaaaaa aaaaaaa R100\0source.rs\0"[..], // rename missing a side
        &b":100644 100644 aaaaaaa aaaaaaa M\0"[..],               // record missing its path
        &b"100644 100644 aaaaaaa aaaaaaa M\0src/lib.rs\0"[..],    // metadata not starting with ':'
        &b"src/lib.rs\0"[..],                                     // a bare path with no metadata
    ] {
        assert!(
            raw_diff_paths(malformed).is_err(),
            "malformed diff record must be an error, not skipped: {:?}",
            String::from_utf8_lossy(malformed)
        );
    }
}

#[test]
fn paths_that_are_not_utf8_are_never_allowlisted() {
    assert!(allowlisted_path(
        b"crates/codegen/grokptah-service/tests/common/mod.rs"
    ));
    assert!(!allowlisted_path(b"outside-resolver-source.rs"));
    let mut invalid = b"crates/codegen/grokptah-service/tests/common/mod.rs".to_vec();
    invalid.push(0xff);
    assert!(
        !allowlisted_path(&invalid),
        "an invalid byte sequence must never be decoded into an allowlisted path"
    );
    assert!(!allowlisted_path(&[0xff, 0xfe]));
}
