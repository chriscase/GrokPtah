//! Installed CLI qualification with real orchestration/relay/check authority.
//! Only the external model response is a loopback fixture; never live xAI.

use super::*;
use crate::orchestration::{
    ManagedGrokExecutorConfig, OrchestrationConfig, OrchestrationService, RunBounds, WorkState,
    WorkspaceAllowlist,
};
use crate::{AgentHost, HostConfig, SessionKind};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use uuid::Uuid;

const FRAMING: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/source/src/framing.py");
const DECODER: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/source/src/decoder.py");
const ORACLE: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/oracle/check.py");
const CHECK: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/oracle/check.sh");
const FIXED_FRAMING: &str = "def encode(message: str) -> bytes:\n    payload = message.encode('utf-8')\n    return str(len(payload)).encode('ascii') + b':' + payload\n";
const FIXED_DECODER: &str = "def decode(frame: bytes) -> str:\n    header, separator, payload = frame.partition(b':')\n    if not separator or header != str(len(payload)).encode('ascii'):\n        raise ValueError('invalid frame')\n    return payload.decode('utf-8')\n";

fn git(source: &Path, args: &[&str]) -> String {
    let result = Command::new("/usr/bin/git")
        .current_dir(source)
        .args(args)
        .output()
        .unwrap();
    assert!(result.status.success());
    String::from_utf8(result.stdout).unwrap().trim().into()
}

async fn call(server: &crate::ControlServerHandle, token: &str, name: &str, args: Value) -> Value {
    let value: Value = reqwest::Client::new().post(format!("http://{}/mcp", server.addr))
        .bearer_auth(token).header("Accept", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}))
        // authority-allow-unauthenticated-wire: Authenticated host-local MCP;
        // the fixture relay retains the canonical upstream wire boundary.
        .send().await.unwrap().json().await.unwrap();
    assert!(
        value.get("error").is_none() && value["result"]["isError"] != true,
        "{value}"
    );
    value["result"]["structuredContent"].clone()
}

fn turn(index: u32, calls: Vec<Value>) -> String {
    turn_with_usage(
        index,
        calls,
        json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}),
    )
}

fn turn_with_usage(index: u32, calls: Vec<Value>, usage: Value) -> String {
    let delta = if calls.is_empty() {
        json!({"role":"assistant","content":"Repaired UTF-8 framing and strict decoding in both files.\nGROK_BUILD_VERDICT=clean"})
    } else {
        json!({"role":"assistant","tool_calls":calls})
    };
    let reason = if delta.get("tool_calls").is_some() {
        "tool_calls"
    } else {
        "stop"
    };
    format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"id":format!("offline-{index}"),"object":"chat.completion.chunk","created":0,"model":"grok-build-0.1","choices":[{"index":0,"delta":delta,"finish_reason":reason}]}),
        json!({"id":format!("offline-{index}"),"object":"chat.completion.chunk","created":0,"model":"grok-build-0.1","choices":[],"usage":usage})
    )
}

fn tool(index: u32, name: &str, args: Value) -> Value {
    json!({"index":index,"id":format!("offline-tool-{name}-{index}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

fn responses_usage(mode: FixtureMode, index: u32) -> Value {
    let mut value = json!({"input_tokens":100,"output_tokens":15,"total_tokens":115,"input_tokens_details":{"cached_tokens":20},"output_tokens_details":{"reasoning_tokens":5},"cost_in_usd_ticks":777,"num_sources_used":0,"num_server_side_tools_used":0});
    match mode {
        FixtureMode::ResponsesOrdinary => {
            value["output_tokens"] = json!(10);
            value["total_tokens"] = json!(110);
            value["output_tokens_details"]["reasoning_tokens"] = json!(0);
            value["input_tokens_details"]["cached_tokens"] = json!(0);
            value.as_object_mut().unwrap().remove("cost_in_usd_ticks");
        }
        FixtureMode::ResponsesReasoning => {
            value["input_tokens_details"]["cached_tokens"] = json!(0)
        }
        FixtureMode::ResponsesAtCap | FixtureMode::ResponsesIncomplete => {
            value["output_tokens"] = json!(1024);
            value["total_tokens"] = json!(1124);
            value["output_tokens_details"]["reasoning_tokens"] = json!(100);
            value["cost_in_usd_ticks"] = json!(0);
        }
        FixtureMode::ResponsesConflicting => value["total_tokens"] = json!(116),
        FixtureMode::ResponsesSubset => {
            value["output_tokens_details"]["reasoning_tokens"] = json!(16)
        }
        FixtureMode::ResponsesUnknown => value["unexplained_charge_ticks"] = json!(123),
        FixtureMode::ResponsesMissing => return Value::Null,
        _ => {}
    }
    if mode == FixtureMode::ResponsesAtCap && index == 0 {
        value["output_tokens"] = json!(15);
        value["total_tokens"] = json!(115);
        value["output_tokens_details"]["reasoning_tokens"] = json!(5);
    }
    value
}

pub(super) fn responses_turn(index: u32, tools: bool, usage: Value, incomplete: bool) -> String {
    let mut items = if tools {
        [("src/framing.py", FIXED_FRAMING), ("src/decoder.py", FIXED_DECODER)].into_iter().enumerate().map(|(i,(path,code))| json!({"type":"function_call","id":format!("fc_{index}_{i}"),"call_id":format!("call_{index}_{i}"),"name":"write","arguments":json!({"file_path":path,"content":code}).to_string(),"status":"completed"})).collect::<Vec<_>>()
    } else {
        vec![
            json!({"type":"message","id":format!("msg_{index}"),"role":"assistant","status":"completed","content":[{"type":"output_text","text":"Repaired both files.\nGROK_BUILD_VERDICT=clean","annotations":[],"logprobs":[]}]}),
        ]
    };
    if tools
        && usage["output_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0
    {
        items.push(json!({"type":"reasoning","id":format!("reason_{index}"),"summary":[{"type":"summary_text","text":"Synthetic bounded file reasoning"}],"encrypted_content":"synthetic-offline-reasoning","status":"completed"}));
    }
    let response = json!({"id":format!("resp_{index}"),"object":"response","created_at":0,"model":"grok-build-0.1","status":if incomplete {"incomplete"} else {"completed"},"max_output_tokens":1024,"output":items,"usage":usage,"error":null,"incomplete_details":if incomplete {json!({"reason":"max_output_tokens"})} else {Value::Null}});
    let mut events = Vec::new();
    let mut created = response.clone();
    created["output"] = json!([]);
    created["usage"] = Value::Null;
    created["status"] = json!("in_progress");
    created["incomplete_details"] = Value::Null;
    events.push(json!({"type":"response.created","response":created}));
    for (i, item) in items.iter().enumerate() {
        let mut added = item.clone();
        added["status"] = json!("in_progress");
        if item["type"] == "function_call" {
            added["arguments"] = json!("");
        }
        events.push(json!({"type":"response.output_item.added","output_index":i,"item":added}));
        if item["type"] == "function_call" {
            events.push(json!({"type":"response.function_call_arguments.delta","output_index":i,"item_id":item["id"],"delta":item["arguments"]}));
            events.push(json!({"type":"response.function_call_arguments.done","output_index":i,"item_id":item["id"],"arguments":item["arguments"],"name":item["name"]}));
        } else if item["type"] == "message" {
            events.push(json!({"type":"response.output_text.delta","output_index":i,"item_id":item["id"],"content_index":0,"delta":item["content"][0]["text"],"logprobs":[]}));
        }
        events.push(json!({"type":"response.output_item.done","output_index":i,"item":item}));
    }
    events.push(json!({"type":if incomplete {"response.incomplete"} else {"response.completed"},"response":response}));
    events
        .into_iter()
        .enumerate()
        .map(|(i, mut v)| {
            v["sequence_number"] = json!(i);
            format!("event: {}\ndata: {v}\n\n", v["type"].as_str().unwrap())
        })
        .collect()
}

async fn shutdown(
    server: crate::ControlServerHandle,
    orch: &OrchestrationService,
    host: &crate::HostRuntime,
) {
    assert!(server.stop_and_wait().await.is_clean());
    let stopped = orch.stop_background_tasks().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    assert!(host.shutdown().await.is_clean());
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FixtureMode {
    Success,
    AccountingCombined,
    Interrupted,
    HttpRejected,
    Malformed,
    MissingUsage,
    UnknownAdditionalUsage,
    AdditiveReasoning,
    ResponsesOrdinary,
    ResponsesReasoning,
    ResponsesCached,
    ResponsesAtCap,
    ResponsesIncomplete,
    ResponsesConflicting,
    ResponsesSubset,
    ResponsesUnknown,
    ResponsesMissing,
    ResponsesMalformed,
}
impl FixtureMode {
    fn responses(self) -> bool {
        self.name().starts_with("responses-")
    }
    fn succeeds(self) -> bool {
        matches!(
            self,
            Self::Success
                | Self::AccountingCombined
                | Self::ResponsesOrdinary
                | Self::ResponsesReasoning
                | Self::ResponsesCached
                | Self::ResponsesAtCap
        )
    }
    fn name(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::AccountingCombined => "accounting-combined",
            Self::Interrupted => "interrupted",
            Self::HttpRejected => "http-rejected",
            Self::Malformed => "malformed",
            Self::MissingUsage => "missing-usage",
            Self::UnknownAdditionalUsage => "unknown-additional-usage",
            Self::AdditiveReasoning => "additive-reasoning",
            Self::ResponsesOrdinary => "responses-a-ordinary",
            Self::ResponsesReasoning => "responses-b-reasoning",
            Self::ResponsesCached => "responses-c-cached",
            Self::ResponsesAtCap => "responses-d-at-cap",
            Self::ResponsesIncomplete => "responses-e-incomplete",
            Self::ResponsesConflicting => "responses-f-conflicting",
            Self::ResponsesSubset => "responses-g-subset",
            Self::ResponsesUnknown => "responses-h-unknown",
            Self::ResponsesMissing => "responses-missing",
            Self::ResponsesMalformed => "responses-malformed",
        }
    }
}

#[allow(clippy::await_holding_lock)]
async fn qualify(mode: FixtureMode) {
    use std::os::unix::fs::PermissionsExt;
    let interrupt = !mode.succeeds();
    let _serial = crate::home_override_serial();
    let root = tempfile::Builder::new()
        .prefix("rma-offline-continuation-")
        .tempdir()
        .unwrap();
    crate::set_grokptah_home_override(Some(root.path().join("host")));
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::set_grokptah_home_override(None);
        }
    }
    let _reset = Reset;
    let cli = PathBuf::from(
        std::env::var("GROKPTAH_REAL_GROK_CLI").expect("installed CLI path required"),
    );
    assert_eq!(
        crate::file_digest(&cli).as_deref(),
        Some("sha256:9c844eb13365180787d9ad22b2b3748a024be8e1ed845253cc114781b31c591d")
    );
    let source = root.path().join("source");
    let oracle = root.path().join("oracle");
    let workers = root.path().join("workers");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir(&oracle).unwrap();
    fs::create_dir(&workers).unwrap();
    fs::set_permissions(&workers, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(source.join("src/framing.py"), FRAMING).unwrap();
    fs::write(source.join("src/decoder.py"), DECODER).unwrap();
    fs::write(oracle.join("check.py"), ORACLE).unwrap();
    fs::write(oracle.join("check.sh"), CHECK).unwrap();
    fs::set_permissions(oracle.join("check.sh"), fs::Permissions::from_mode(0o700)).unwrap();
    git(&source, &["init", "-b", "offline-qualification"]);
    git(&source, &["config", "user.name", "Offline qualification"]);
    git(
        &source,
        &["config", "user.email", "offline@grokptah.invalid"],
    );
    git(&source, &["add", "src"]);
    git(&source, &["commit", "-m", "Red offline fixture"]);
    let base = git(&source, &["rev-parse", "HEAD"]);
    let base_tree = git(&source, &["rev-parse", "HEAD^{tree}"]);
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..Default::default()
    })
    .unwrap();
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(lane.id, &source).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let token = Uuid::new_v4().to_string();
    let config = || OrchestrationConfig {
        bearer_token: token.clone(),
        allowlist: WorkspaceAllowlist::new([source.clone()]),
        max_concurrent_runs: 1,
        bounds: RunBounds {
            max_prompt_bytes: 16000,
            max_rounds: 6,
            max_duration_ms: 180000,
            max_total_tokens: Some(MAX_TOTAL_TOKENS),
        },
    };
    let store_root = root.path().join("host/orchestration");
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        host.ensure_orchestration_store().unwrap(),
        config(),
    );
    let profiles = store_root.join("check-profiles");
    fs::create_dir_all(&profiles).unwrap();
    let python = [
        "/Applications/Xcode.app/Contents/Developer/usr/bin/python3",
        "/Library/Developer/CommandLineTools/usr/bin/python3",
    ]
    .into_iter()
    .find(|p| Path::new(p).is_file())
    .unwrap();
    fs::write(profiles.join("offline-utf8.json"), serde_json::to_vec(&json!({"profileId":"offline-utf8","profileRevision":1,"executable":oracle.join("check.sh"),"executableDigest":crate::file_digest(&oracle.join("check.sh")),"oracleRoot":oracle,"oracleDigest":crate::directory_digest(&oracle),"args":[],"cwd":"oracle","env":[{"key":"QUALIFICATION_PYTHON","value":python}],"timeoutMs":5000,"maxOutputBytes":16384,"maxOutputDirBytes":65536,"network":"none","confinementRevision":2})).unwrap()).unwrap();
    let (check, mut authority, oracle_root) =
        crate::resolve_check_profile(&store_root, "offline-utf8").unwrap();
    authority.source_root = source.clone();
    let authority = authority.seal();
    let red = crate::execute_required_checks_with_authority(
        &[check],
        &source,
        &oracle_root,
        Some(&authority),
    )
    .unwrap();
    assert_eq!(
        (red[0].outcome.as_str(), red[0].exit_code),
        ("failed", Some(1))
    );
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let shapes = Arc::new(Mutex::new(Vec::<Value>::new()));
    let shape_capture = shapes.clone();
    let router = Router::new().route(if mode.responses() {"/v1/responses"} else {"/v1/chat/completions"}, post(move |headers: HeaderMap, Json(body): Json<Value>| {
        let count = count.clone(); let shape_capture = shape_capture.clone();
        async move {
            assert_eq!(headers["authorization"], "Bearer upstream-test-secret-never-child");
            if mode.responses() {
                assert_eq!(headers["x-xai-token-auth"], "xai-grok-cli");
                assert_eq!(headers["x-grok-model-override"], "grok-build-0.1");
                assert_eq!(body["max_output_tokens"], 1024);
                assert_eq!(body["reasoning"], json!({"summary":"concise"}));
                assert!(body.get("max_tokens").is_none() && body.get("max_completion_tokens").is_none());
            } else { assert!(headers.contains_key("idempotency-key")); }
            let index = count.fetch_add(1, Ordering::SeqCst);
            // Retain only field/tool/schema names and numeric budget facts.
            shape_capture.lock().unwrap().push(json!({"fields":body.as_object().unwrap().keys().collect::<Vec<_>>(),"maxTokens":body["max_tokens"],"maxOutputTokens":body["max_output_tokens"],"model":body["model"],"reasoning":body["reasoning"],"oidcHeader":headers.get("x-xai-token-auth").and_then(|v| v.to_str().ok()),"modelRoutingHeader":headers.get("x-grok-model-override").and_then(|v| v.to_str().ok()),"inputTypes":body["input"].as_array().map(|a| a.iter().map(|i| i["type"].as_str().unwrap_or("message")).collect::<Vec<_>>()),"functionToolResults":body["input"].as_array().map(|a| a.iter().filter(|i| i["type"] == "function_call_output").count()),"requestBytes":body.to_string().len(),"tools":body["tools"].as_array().unwrap().iter().map(|t| json!({"name":if mode.responses() {&t["name"]} else {&t["function"]["name"]},"parameterKeys":(if mode.responses() {&t["parameters"]["properties"]} else {&t["function"]["parameters"]["properties"]}).as_object().map(|o| o.keys().cloned().collect::<Vec<_>>())})).collect::<Vec<_>>()}));
            if mode.responses() {
                assert!(body["tools"].as_array().unwrap().iter().all(|t| t["type"] == "function"));
                if index == 1 { assert_eq!(body["input"].as_array().unwrap().iter().filter(|i| i["type"] == "function_call_output").count(), 2); }
                let response = if mode == FixtureMode::ResponsesMalformed { "data: SECRET_RESPONSE_CANARY\n\n".into() } else {responses_turn(index,index == 0 && mode.succeeds(),responses_usage(mode,index),mode == FixtureMode::ResponsesIncomplete)};
                return ([("content-type","text/event-stream")],response).into_response();
            }
            if mode == FixtureMode::Interrupted {
                use futures::StreamExt;
                let partial = futures::stream::once(async { Ok::<_, std::io::Error>(Bytes::from_static(b"data: {\"choices\":[]}")) });
                let failure = futures::stream::once(async { Err::<Bytes,_>(std::io::Error::other("SECRET_TRANSPORT_CANARY")) });
                return ([("content-type","text/event-stream"),("x-request-id","12345678-1234-4234-8234-123456789abc")], axum::body::Body::from_stream(partial.chain(failure))).into_response();
            }
            if mode == FixtureMode::HttpRejected {
                return (StatusCode::UNAUTHORIZED, [("x-request-id", "12345678-1234-4234-8234-123456789abc"),("set-cookie", "COOKIE_CANARY")], Json(json!({"error":{"type":"authentication_error","code":"invalid_api_key","message":"SECRET_RESPONSE_CANARY"}}))).into_response();
            }
            if matches!(mode, FixtureMode::Malformed | FixtureMode::MissingUsage) {
                let body = if mode == FixtureMode::Malformed { "data: SECRET_RESPONSE_CANARY\n\n".into() } else { turn(index, vec![]).replace("\"usage\":", "\"omitted_usage\":") };
                return ([("content-type", "text/event-stream")], body).into_response();
            }
            if mode == FixtureMode::UnknownAdditionalUsage {
                return ([("content-type", "text/event-stream")], turn_with_usage(index, vec![], json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"additional_charge_ticks":123}))).into_response();
            }
            if mode == FixtureMode::AdditiveReasoning {
                // The public Chat Completions example can put reasoning beyond
                // completion. CLI 1.0.41 cannot reconcile that total, so the
                // host must reject this receipt before declaring success.
                return ([("content-type", "text/event-stream")], turn_with_usage(index, vec![], json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":115,"prompt_tokens_details":{"text_tokens":100,"audio_tokens":0,"image_tokens":0,"cached_tokens":20},"completion_tokens_details":{"reasoning_tokens":5,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},"cost_in_usd_ticks":777}))).into_response();
            }
            let tools = match index {
                0 => vec![tool(0,"write",json!({"file_path":"src/framing.py","content":FIXED_FRAMING})),tool(1,"write",json!({"file_path":"src/decoder.py","content":FIXED_DECODER}))],
                1 => vec![],
                _ => panic!("offline model fixture exceeded the specified two turns"),
            };
            let response = if mode == FixtureMode::AccountingCombined {
                let (cached, reasoning, ticks) = if index == 0 { (20, 3, 777) } else { (40, 4, 888) };
                turn_with_usage(index, tools, json!({"prompt_tokens":100,"completion_tokens":10,"total_tokens":110,"prompt_tokens_details":{"text_tokens":100,"audio_tokens":0,"image_tokens":0,"cached_tokens":cached},"completion_tokens_details":{"reasoning_tokens":reasoning,"audio_tokens":0,"accepted_prediction_tokens":0,"rejected_prediction_tokens":0},"cost_in_usd_ticks":ticks,"num_sources_used":0}))
            } else { turn(index, tools) };
            ([("content-type","text/event-stream"),("x-request-id","12345678-1234-4234-8234-123456789abc")], response).into_response()
        }
    }));
    let (relay, upstream) = super::tests::local_fixture_with_backend(
        root.path().join("leases"),
        router,
        if mode.responses() {
            ManagedBackend::Responses
        } else {
            ManagedBackend::ChatCompletions
        },
    )
    .await;
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: cli.clone(),
            git_executable: "/usr/bin/git".into(),
            cwd: source.clone(),
            isolate_parent: workers.clone(),
            repository_id: "offline:utf8-frame-continuation".into(),
            base_ref: "refs/heads/offline-qualification".into(),
            identity: grokptah_agent_sdk::GrokBuildGitIdentity {
                repository_id: "offline:utf8-frame-continuation".into(),
                git_ref: "refs/heads/offline-qualification".into(),
                base_sha: base.clone(),
                head_sha: base.clone(),
            },
            credential_lease_id: "offline-continuation-only".into(),
        },
        relay.clone(),
    )
    .unwrap();
    let server = crate::start_control_server(orch.clone(), 0).await.unwrap();
    let args = json!({"request_id":Uuid::new_v4().to_string(),"session_id":lane.id,"workspace":source,"agent_id":agent.agent_id,"objective":"Repair UTF-8 framing and strict canonical decoding in the two allowed files; finish with GROK_BUILD_VERDICT=clean.","allowed_files":["src/framing.py","src/decoder.py"],"check_profile_id":"offline-utf8","budget_profile":"economy","mutation_mode":"isolated_review"});
    let readiness = call(
        &server,
        &token,
        "ptah_prepare_verified_change",
        args.clone(),
    )
    .await;
    assert_eq!(readiness["readiness"]["ready"], true, "{readiness}");
    let started = call(&server, &token, "ptah_start_verified_change", args).await;
    let work_id = started["workId"].as_str().unwrap().to_string();
    let deadline = Instant::now();
    loop {
        orch.drive_native_executor_once().await;
        let work = orch.store().load_work_item(&work_id).unwrap().unwrap();
        if !matches!(
            work.state,
            WorkState::Queued | WorkState::Leased | WorkState::Running
        ) {
            break;
        }
        assert!(
            deadline.elapsed() < Duration::from_secs(195),
            "offline qualification did not settle"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status_args = json!({"request_id":"offline-status","session_id":lane.id,"workspace":source,"work_id":work_id});
    let review = call(
        &server,
        &token,
        "ptah_verified_change_status",
        status_args.clone(),
    )
    .await;
    let attempts = orch.store().list_work_attempts(Some(&work_id)).unwrap();
    let runs = orch.store().list_runs().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(runs.len(), 1);
    assert_eq!(git(&source, &["rev-parse", "HEAD"]), base);
    assert!(git(&source, &["diff", "--stat"]).is_empty());
    let classification_refs = orch
        .store()
        .list_managed_intents()
        .unwrap()
        .into_iter()
        .find(|intent| intent.work_id == work_id)
        .unwrap()
        .grok
        .unwrap()
        .evidence_refs;
    if interrupt {
        assert_ne!(review["phases"]["checksPassed"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    } else {
        assert_eq!(
            review["phases"]["checksPassed"],
            true,
            "{review}; classification refs: {classification_refs:?}; safe request shapes: {:?}",
            *shapes.lock().unwrap()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let candidate = orch.store().candidate_snapshot_dir(&work_id).unwrap();
        assert_eq!(
            fs::read_to_string(candidate.join("src/framing.py")).unwrap(),
            FIXED_FRAMING
        );
        assert_eq!(
            fs::read_to_string(candidate.join("src/decoder.py")).unwrap(),
            FIXED_DECODER
        );
    }
    let evidence = &review["providerEvidence"];
    assert_eq!(evidence["wireAttempts"], if interrupt { 1 } else { 2 });
    let expected_kind = match mode {
        FixtureMode::Success
        | FixtureMode::AccountingCombined
        | FixtureMode::ResponsesOrdinary
        | FixtureMode::ResponsesReasoning
        | FixtureMode::ResponsesCached
        | FixtureMode::ResponsesAtCap => None,
        FixtureMode::ResponsesIncomplete => Some("output_limit_incomplete"),
        FixtureMode::ResponsesConflicting
        | FixtureMode::ResponsesSubset
        | FixtureMode::ResponsesUnknown => Some("usage_inconsistent"),
        FixtureMode::ResponsesMissing => Some("usage_missing"),
        FixtureMode::ResponsesMalformed => Some("protocol_failure"),
        FixtureMode::Interrupted => Some("transport_uncertain"),
        FixtureMode::HttpRejected => Some("http_rejected"),
        FixtureMode::Malformed => Some("protocol_failure"),
        FixtureMode::MissingUsage => Some("usage_missing"),
        FixtureMode::UnknownAdditionalUsage | FixtureMode::AdditiveReasoning => {
            Some("usage_inconsistent")
        }
    };
    if let Some(kind) = expected_kind {
        assert_eq!(evidence["interruption"], kind, "{evidence}");
        assert_eq!(
            evidence["accountingComplete"],
            mode == FixtureMode::ResponsesIncomplete
        );
        assert_eq!(
            evidence["remoteEffectUncertain"],
            mode != FixtureMode::ResponsesIncomplete
        );
        let expected_subreason = match mode {
            FixtureMode::MissingUsage | FixtureMode::ResponsesMissing => Some("missing_receipt"),
            FixtureMode::ResponsesConflicting => Some("conflicting_total"),
            FixtureMode::ResponsesSubset => Some("conflicting_subset"),
            FixtureMode::ResponsesUnknown => Some("unsupported_field"),
            FixtureMode::UnknownAdditionalUsage => Some("unsupported_field"),
            FixtureMode::AdditiveReasoning => Some("conflicting_total"),
            _ => None,
        };
        if let Some(subreason) = expected_subreason {
            let diagnostic = evidence["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["kind"] == kind)
                .unwrap();
            assert_eq!(diagnostic["usageRejection"]["kind"], subreason);
            if mode == FixtureMode::AdditiveReasoning {
                assert_eq!(diagnostic["usageRejection"]["field"], "total_tokens");
                assert_eq!(diagnostic["usageRejection"]["observed"], 115);
                assert_eq!(diagnostic["usageRejection"]["expected"], 110);
                let snapshot = &evidence["usageObservation"]["snapshots"][0];
                let fields = snapshot["fields"].as_array().unwrap();
                let observed = |path: &str| fields.iter().find(|f| f["path"] == path).unwrap();
                assert_eq!(observed("prompt_tokens")["value"], 100);
                assert_eq!(observed("completion_tokens")["value"], 10);
                assert_eq!(observed("total_tokens")["value"], 115);
                assert_eq!(observed("prompt_tokens_details.cached_tokens")["value"], 20);
                assert_eq!(
                    observed("completion_tokens_details.reasoning_tokens")["value"],
                    5
                );
                assert_eq!(observed("cost_in_usd_ticks")["value"], 777);
                assert_eq!(evidence["responsesCompleted"], 0);
                assert_eq!(evidence["revoked"], true);
                assert_eq!(evidence["remoteEffectUncertain"], true);
            }
            assert!(!evidence.to_string().contains("additional_charge_ticks"));
        }
    } else {
        assert_eq!(evidence["accountingComplete"], true);
        assert!(evidence["interruption"].is_null());
        if mode.responses() {
            let expected = responses_usage(mode, 1);
            let first = responses_usage(mode, 0);
            assert_eq!(evidence["inputTokens"], 200);
            assert_eq!(
                evidence["outputTokens"].as_u64(),
                Some(
                    first["output_tokens"].as_u64().unwrap()
                        + expected["output_tokens"].as_u64().unwrap()
                )
            );
            assert_eq!(
                evidence["totalTokens"].as_u64(),
                Some(
                    first["total_tokens"].as_u64().unwrap()
                        + expected["total_tokens"].as_u64().unwrap()
                )
            );
            assert_eq!(
                evidence["reasoningTokens"].as_u64(),
                Some(
                    first["output_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap()
                        + expected["output_tokens_details"]["reasoning_tokens"]
                            .as_u64()
                            .unwrap()
                )
            );
            assert_eq!(
                evidence["costComplete"],
                mode != FixtureMode::ResponsesOrdinary
            );
            assert_eq!(
                evidence["costInUsdTicks"].as_i64(),
                expected["cost_in_usd_ticks"].as_i64().map(|c| c * 2)
            );
        }
        if mode == FixtureMode::AccountingCombined {
            assert_eq!(evidence["inputTokens"], 200);
            assert_eq!(evidence["outputTokens"], 20);
            assert_eq!(evidence["totalTokens"], 220);
            assert_eq!(evidence["cacheReadInputTokens"], 60);
            assert_eq!(evidence["reasoningTokens"], 7);
            assert_eq!(evidence["costInUsdTicks"], 1665);
            assert_eq!(evidence["costMissingCalls"], 0);
            assert_eq!(evidence["costComplete"], true);
        }
    }
    if mode == FixtureMode::HttpRejected {
        assert_eq!(evidence["diagnostics"][0]["httpStatus"], 401);
        assert_eq!(
            evidence["diagnostics"][1]["providerErrorCode"],
            "invalid_api_key"
        );
    }
    let initial_count = calls.load(Ordering::SeqCst);
    shutdown(server, &orch, &host).await;
    drop(orch);
    drop(host);
    let mut reopens = Vec::new();
    for reopen in 0..3 {
        let host = AgentHost::create(HostConfig {
            always_approve: false,
            ..Default::default()
        })
        .unwrap();
        host.start().unwrap();
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            host.ensure_orchestration_store().unwrap(),
            config(),
        );
        let server = crate::start_control_server(orch.clone(), 0).await.unwrap();
        orch.drive_native_executor_once().await;
        let status = call(
            &server,
            &token,
            "ptah_verified_change_status",
            status_args.clone(),
        )
        .await;
        assert_eq!(
            orch.store()
                .list_work_attempts(Some(&work_id))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(orch.store().list_runs().unwrap().len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), initial_count);
        assert_eq!(status["providerEvidence"], *evidence);
        if reopen == 0 && !mode.responses() {
            assert_eq!(status["candidateDigest"], review["candidateDigest"]);
            assert!(git(&source, &["diff", "--stat"]).is_empty());
            if !interrupt {
                let work = orch.store().load_work_item(&work_id).unwrap().unwrap();
                call(&server,&token,"ptah_approve_work",json!({"request_id":"offline-explicit-approve","session_id":lane.id,"workspace":source,"work_id":work_id,"expected_revision":work.revision,"note":"Explicit disposition of the inspected offline candidate"})).await;
            }
            let work = orch.store().load_work_item(&work_id).unwrap().unwrap();
            call(&server,&token,if interrupt {"ptah_discard_verified_change"} else {"ptah_apply_verified_change"},json!({"request_id":"offline-explicit-disposition","session_id":lane.id,"workspace":source,"work_id":work_id,"candidate_digest":status["candidateDigest"],"expected_revision":work.revision})).await;
        }
        let after = call(
            &server,
            &token,
            "ptah_verified_change_status",
            status_args.clone(),
        )
        .await;
        if interrupt || mode.responses() {
            assert!(git(&source, &["diff", "--stat"]).is_empty());
        } else {
            assert_eq!(
                fs::read_to_string(source.join("src/framing.py")).unwrap(),
                FIXED_FRAMING
            );
            assert_eq!(
                fs::read_to_string(source.join("src/decoder.py")).unwrap(),
                FIXED_DECODER
            );
        }
        reopens.push(after);
        shutdown(server, &orch, &host).await;
        drop(orch);
        drop(host);
    }
    assert!(fs::read_dir(&workers).unwrap().next().is_none());
    fn scan(path: &Path) {
        if path.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                scan(&entry.unwrap().path());
            }
        } else if path.is_file() {
            let bytes = fs::read(path).unwrap();
            for canary in [
                "upstream-test-secret-never-child",
                "SECRET_TRANSPORT_CANARY",
                "SECRET_RESPONSE_CANARY",
                "COOKIE_CANARY",
            ] {
                assert!(
                    !bytes
                        .windows(canary.len())
                        .any(|part| part == canary.as_bytes()),
                    "retained canary at {}",
                    path.display()
                );
            }
        }
    }
    scan(root.path());
    fn retained_fields<T: serde::Serialize>(records: &[T], fields: &[&str]) -> Vec<Value> {
        records
            .iter()
            .map(|record| {
                let mut value = serde_json::to_value(record).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .retain(|key, _| fields.contains(&key.as_str()));
                value
            })
            .collect()
    }
    let safe_runs = retained_fields(
        &runs,
        &[
            "runId",
            "agentId",
            "agentSpecRevision",
            "sessionId",
            "state",
            "bounds",
            "execution",
            "stopCause",
            "errorCode",
            "retryOf",
        ],
    );
    let safe_attempts = retained_fields(
        &attempts,
        &[
            "attemptId",
            "attemptNumber",
            "workId",
            "state",
            "linkedRunIds",
        ],
    );
    let report = json!({"schemaVersion":1,"fixtureMode":mode.name(),"qualification":"OFFLINE ONLY; not live G4/G5/G8","interrupted":interrupt,"baseSha":base,"baseTree":base_tree,"cliDigest":crate::file_digest(&cli),"redBaseline":red,"readiness":readiness,"workId":work_id,"attempts":safe_attempts,"runs":safe_runs,"review":review,"reopens":reopens,"fixturePhysicalRequests":initial_count,"additionalLiveRequests":0,"requestShapes":*shapes.lock().unwrap(),"workerRootEmpty":fs::read_dir(&workers).unwrap().next().is_none()});
    let encoded = serde_json::to_string_pretty(&report).unwrap();
    for secret in [
        "upstream-test-secret-never-child",
        "SECRET_TRANSPORT_CANARY",
    ] {
        assert!(!encoded.contains(secret));
    }
    if let Ok(path) = std::env::var("GROKPTAH_OFFLINE_REPORT") {
        fs::write(format!("{path}-{}.json", mode.name()), encoded).unwrap();
    }
    upstream.abort();
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_managed_two_file_candidate_checks_apply_and_reopen_offline() {
    qualify(FixtureMode::Success).await;
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_combined_accounting_survives_candidate_and_reopen_offline() {
    qualify(FixtureMode::AccountingCombined).await;
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn interrupted_forward_recovery_never_creates_another_paid_attempt() {
    qualify(FixtureMode::Interrupted).await;
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_http_rejection_diagnostics_survive_reopen() {
    qualify(FixtureMode::HttpRejected).await;
}
#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_protocol_failure_diagnostics_survive_reopen() {
    qualify(FixtureMode::Malformed).await;
}
#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_missing_usage_diagnostics_survive_reopen() {
    qualify(FixtureMode::MissingUsage).await;
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_unknown_additional_usage_fails_closed_and_survives_reopen() {
    qualify(FixtureMode::UnknownAdditionalUsage).await;
}

#[tokio::test]
#[ignore = "requires installed Grok 1.0.41 and native confinement; offline fixture only"]
async fn installed_cli_additive_reasoning_fails_closed_and_survives_reopen() {
    qualify(FixtureMode::AdditiveReasoning).await;
}

#[tokio::test]
#[ignore = "requires pinned CLI and native sandbox; localhost only"]
async fn installed_cli_responses_managed_journey_and_accounting_matrix_offline() {
    for mode in [
        FixtureMode::ResponsesOrdinary,
        FixtureMode::ResponsesReasoning,
        FixtureMode::ResponsesCached,
        FixtureMode::ResponsesAtCap,
        FixtureMode::ResponsesIncomplete,
        FixtureMode::ResponsesConflicting,
        FixtureMode::ResponsesSubset,
        FixtureMode::ResponsesUnknown,
        FixtureMode::ResponsesMissing,
        FixtureMode::ResponsesMalformed,
    ] {
        qualify(mode).await;
    }
}
