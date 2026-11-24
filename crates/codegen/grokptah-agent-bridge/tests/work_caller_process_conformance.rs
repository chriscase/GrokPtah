//! Production caller admission, process death and original outcome conformance.
mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    canonical_workspace, safe_id_filename, AuthContext, ManagedWorkMode, OrchError, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, RunRecord, RunState, WorkAttempt,
    WorkAttemptView, WorkPolicy, WorkResult, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, DurableWriteFault, HostConfig, SessionKind,
    DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use uuid::Uuid;

const TOKEN: &str = "synthetic-v2-process-caller";
const REQUEST: &str = "original-production-caller";
const OPERATIONS: &[&str] = &[
    "claim",
    "renew",
    "link",
    "assign",
    "offer",
    "accept",
    "decline",
    "reassign",
    "priority",
    "block",
    "unblock",
    "review",
    "authorize",
    "progress",
    "release",
    "complete",
    "fail",
    "cancel",
    "retry",
    "approve",
];
fn lifecycle(op: &str) -> bool {
    matches!(
        op,
        "claim"
            | "renew"
            | "link"
            | "assign"
            | "progress"
            | "release"
            | "complete"
            | "fail"
            | "cancel"
            | "retry"
            | "approve"
    )
}
fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut result = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "json")
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    result.sort();
    result
}
fn receipt_path(root: &Path) -> PathBuf {
    let name = format!("{}.json", safe_id_filename(REQUEST).unwrap());
    walkdir::WalkDir::new(root.join("home/ledger/idempotency"))
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry
                .path()
                .file_name()
                .is_some_and(|file| file == name.as_str())
        })
        .unwrap()
        .into_path()
}
fn receipt(root: &Path) -> Value {
    read(receipt_path(root))
}
fn child(root: &Path, op: &str, phase: &str, stage: &str) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "production_caller_child", "--nocapture"])
        .env("CALLER_ROOT", root)
        .env("CALLER_OP", op)
        .env("CALLER_PHASE", phase)
        .env("CALLER_STAGE", stage)
        .env_remove("GROKPTAH_CRASH_STAGE")
        .output()
        .unwrap()
}
fn exited(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn result(reason: &str, failed: bool) -> WorkResult {
    serde_json::from_value(json!({"summary":reason,"evidence":["synthetic evidence"],"artifacts":[],"failure":failed.then_some("synthetic failure"),"completedAt":Utc::now()})).unwrap()
}
fn linked_run(session: Uuid, workspace: &str) -> RunRecord {
    let now = Utc::now();
    RunRecord {
        run_id: "synthetic-linked-run".into(),
        session_id: session,
        workspace: workspace.into(),
        request_id: "synthetic-run-fixture".into(),
        client_id: None,
        state: RunState::Running,
        purpose: Default::default(),
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "synthetic run fixture".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: now,
        updated_at: now,
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    }
}
fn token(store: &OrchStore, args: &Value) -> String {
    store
        .list_work_attempts(None)
        .unwrap()
        .into_iter()
        .find(|attempt| json!(attempt.attempt_id) == args["attemptId"])
        .unwrap()
        .lease_token_for_secret(TOKEN)
}
async fn invoke(
    orch: &OrchestrationService,
    store: &OrchStore,
    auth: &AuthContext,
    args: &Value,
    op: &str,
) -> Result<Value, OrchError> {
    let session = Uuid::parse_str(args["sessionId"].as_str().unwrap()).unwrap();
    let ws = Path::new(args["workspace"].as_str().unwrap());
    let work = args["workId"].as_str().unwrap();
    let revision = args["expectedRevision"].as_u64();
    let agent = args["agentId"].as_str().unwrap();
    let reason = args["reason"].as_str().unwrap().to_string();
    let attempt = args["attemptId"].as_str().unwrap_or("");
    let lease = if attempt.is_empty() {
        String::new()
    } else {
        token(store, args)
    };
    match op {
        "claim" => {
            orch.claim_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                args["leaseMs"].as_u64(),
                None,
            )
            .await
        }
        "renew" => {
            orch.renew_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                attempt,
                &lease,
                args["leaseMs"].as_u64(),
            )
            .await
        }
        "link" => {
            orch.link_work_run(
                auth,
                REQUEST,
                session,
                ws,
                work,
                attempt,
                &lease,
                args["runId"].as_str().unwrap(),
            )
            .await
        }

        "assign" => {
            orch.assign_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                Some(agent.into()),
                revision,
            )
            .await
        }
        "offer" => {
            orch.offer_work(
                auth, REQUEST, session, ws, work, agent, reason, revision, None,
            )
            .await
        }
        "accept" => {
            orch.accept_work(auth, REQUEST, session, ws, work, agent, reason, revision)
                .await
        }
        "decline" => {
            orch.decline_work(auth, REQUEST, session, ws, work, agent, reason, revision)
                .await
        }
        "reassign" => {
            orch.reassign_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                args["otherAgentId"].as_str().unwrap(),
                reason,
                revision,
                None,
            )
            .await
        }
        "priority" => {
            orch.reprioritize_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                args["priority"].as_i64().unwrap() as i32,
                reason,
                revision,
            )
            .await
        }
        "block" => {
            orch.block_work(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "unblock" => {
            orch.unblock_work(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "review" => {
            orch.request_work_review(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "authorize" => {
            orch.authorize_work_execution(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "progress" => {
            orch.report_work_progress(
                auth,
                REQUEST,
                session,
                ws,
                work,
                attempt,
                &lease,
                reason,
                Some(40),
            )
            .await
        }
        "release" => {
            orch.release_work(auth, REQUEST, session, ws, work, attempt, &lease, reason)
                .await
        }
        "complete" => {
            orch.complete_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                attempt,
                &lease,
                result(&reason, false),
            )
            .await
        }
        "fail" => {
            orch.fail_work(
                auth,
                REQUEST,
                session,
                ws,
                work,
                attempt,
                &lease,
                result(&reason, true),
            )
            .await
        }
        "cancel" => {
            orch.cancel_work(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "retry" => {
            orch.retry_work(auth, REQUEST, session, ws, work, reason, revision)
                .await
        }
        "approve" => {
            orch.approve_work(auth, REQUEST, session, ws, work, Some(reason), revision)
                .await
        }
        _ => panic!("unknown caller {op}"),
    }
}
#[test]
fn production_caller_child() {
    let Ok(root) = std::env::var("CALLER_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    set_grokptah_home_override(Some(root.join("home")));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_child(root, &mut env));
}
async fn run_child(root: PathBuf, env: &mut common::ProcessEnvGuard) {
    let op = std::env::var("CALLER_OP").unwrap();
    let phase = std::env::var("CALLER_PHASE").unwrap();
    let stage = std::env::var("CALLER_STAGE").unwrap();
    std::fs::create_dir_all(root.join("workspace")).unwrap();
    let ws = canonical_workspace(&root.join("workspace")).unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let setup = !root.join("fixture.json").exists();
    let session = if setup {
        host.session_new_kind(SessionKind::Build).unwrap().id
    } else {
        let id = Uuid::parse_str(
            read(root.join("fixture.json"))["sessionId"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        host.session_inspect(id).unwrap();
        id
    };
    host.session_set_cwd(session, &ws).unwrap();
    let store = OrchStore::open(root.join("home/ledger")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([ws.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    // Stop unrelated autonomous fixture writers; the actual service/store
    // mutation still owns all admission, authentication and durable writes.
    assert!(orch.stop_background_tasks().await.fully_stopped);
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let args = if setup {
        let agent = host.ensure_session_agent(session).unwrap();
        let mut other = agent.clone();
        other.agent_id = "synthetic-second-worker".into();
        other.spec = None;
        store.save_agent(&other).unwrap();
        let mut policy = WorkPolicy {
            managed_execution: ManagedWorkMode::Forbid,
            ..WorkPolicy::default()
        };
        policy.requires_approval = op == "approve";
        policy.retry.retry_failed = false;
        let created = orch
            .create_work(
                &auth,
                "setup-create",
                session,
                &ws,
                "test".into(),
                "production caller fixture".into(),
                0,
                None,
                None,
                Vec::new(),
                policy,
            )
            .await
            .unwrap();
        let work = created["work"]["workId"].as_str().unwrap();
        if matches!(op.as_str(), "accept" | "decline" | "reassign") {
            orch.offer_work(
                &auth,
                "setup-offer",
                session,
                &ws,
                work,
                &agent.agent_id,
                "synthetic prelude offer".into(),
                Some(1),
                None,
            )
            .await
            .unwrap();
            if op == "reassign" {
                let revision = store.load_work_item(work).unwrap().unwrap().revision;
                orch.accept_work(
                    &auth,
                    "setup-accept",
                    session,
                    &ws,
                    work,
                    &agent.agent_id,
                    "synthetic prelude accept".into(),
                    Some(revision),
                )
                .await
                .unwrap();
            }
        }
        if op == "unblock" {
            orch.block_work(
                &auth,
                "setup-block",
                session,
                &ws,
                work,
                "synthetic prelude block".into(),
                Some(1),
            )
            .await
            .unwrap();
        }
        let mut attempt = Value::Null;
        if matches!(
            op.as_str(),
            "renew"
                | "link"
                | "progress"
                | "release"
                | "complete"
                | "fail"
                | "cancel"
                | "retry"
                | "approve"
                | "review"
        ) {
            let claim = orch
                .claim_work(
                    &auth,
                    "setup-claim",
                    session,
                    &ws,
                    work,
                    Some(300_000),
                    None,
                )
                .await
                .unwrap();
            attempt = claim["attempt"]["attemptId"].clone();
            let lease = claim["leaseToken"].as_str().unwrap();
            let typed = store.list_work_attempts(Some(work)).unwrap().pop().unwrap();
            assert_eq!(typed.lease_token_for_secret(TOKEN), lease);
            if op == "retry" {
                orch.fail_work(
                    &auth,
                    "setup-fail",
                    session,
                    &ws,
                    work,
                    attempt.as_str().unwrap(),
                    lease,
                    result("prelude failed", true),
                )
                .await
                .unwrap();
            }
            if matches!(op.as_str(), "approve" | "review") {
                orch.complete_work(
                    &auth,
                    "setup-complete",
                    session,
                    &ws,
                    work,
                    attempt.as_str().unwrap(),
                    lease,
                    result("prelude complete", false),
                )
                .await
                .unwrap();
            }
        }
        if op == "link" {
            store
                .save_run(&linked_run(session, &ws.display().to_string()))
                .unwrap();
        }
        let current = store.load_work_item(work).unwrap().unwrap();
        let args = json!({"sessionId":session,"workspace":ws,"workId":work,"agentId":agent.agent_id,"otherAgentId":other.agent_id,"attemptId":attempt,"expectedRevision":current.revision,"reason":"original mutation","priority":7,"leaseMs":600000,"runId":"synthetic-linked-run"});
        write(root.join("fixture.json"), &args);
        write(root.join("prior-work.json"), &json!(current));
        args
    } else {
        read(root.join("fixture.json"))
    };
    if phase == "cut" {
        env.set("GROKPTAH_CRASH_STAGE", stage.as_str());
    }
    if phase == "fault" {
        let (needle, skips_remaining) = match stage.as_str() {
            "before_intent" => (
                if lifecycle(&op) {
                    "work-lifecycle-intents"
                } else {
                    "work-intents"
                },
                0,
            ),
            "attempt" => ("work-attempts", 0),
            "decision" => ("work-decisions", 0),
            "item" => ("work-items", 0),
            "receipt" => ("idempotency/v2", 0),
            "cleanup" => (
                if lifecycle(&op) {
                    "work-lifecycle-intents"
                } else {
                    "work-intents"
                },
                1,
            ),
            _ => panic!("unknown fault {stage}"),
        };
        *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
            needle,
            skips_remaining,
        });
    }
    let outcome = match invoke(&orch, &store, &auth, &args, &op).await {
        Ok(response) => json!({"response":response}),
        Err(error) => json!({"error":error}),
    };
    write(root.join(format!("{phase}.json")), &outcome);
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
    if phase == "refusal" || phase == "fault" {
        assert!(
            outcome.get("error").is_some(),
            "expected {op}/{stage} refusal: {outcome}"
        );
        if phase == "fault" {
            assert_eq!(
                store.ledger_admission_closed().is_some(),
                stage != "before_intent"
            );
            if stage != "before_intent" {
                assert!(orch
                    .create_work(
                        &auth,
                        "later-must-not-admit",
                        session,
                        &ws,
                        "test".into(),
                        "refuse after uncertainty".into(),
                        0,
                        None,
                        None,
                        Vec::new(),
                        WorkPolicy::default()
                    )
                    .await
                    .is_err());
            }
        }
    } else {
        assert!(
            outcome.get("response").is_some(),
            "{op} caller failed: {outcome}"
        );
        let original = receipt(&root);
        let mut changed = args.clone();
        changed["expectedRevision"] = json!(args["expectedRevision"].as_u64().unwrap() + 1);
        changed["reason"] = json!("changed mutation");
        changed["leaseMs"] = json!(600001);
        changed["runId"] = json!("wrong-linked-run");

        assert!(invoke(&orch, &store, &auth, &changed, &op).await.is_err());
        assert_eq!(receipt(&root), original);
        if phase == "replay" {
            for field in ["workId", "sessionId", "workspace"] {
                let mut foreign = args.clone();
                foreign[field] = match field {
                    "workId" => json!("wrong-target-work"),
                    "sessionId" => json!(Uuid::new_v4()),
                    _ => {
                        std::fs::create_dir_all(root.join("foreign-workspace")).unwrap();
                        json!(root.join("foreign-workspace"))
                    }
                };
                assert!(
                    invoke(&orch, &store, &auth, &foreign, &op).await.is_err(),
                    "{op} accepted foreign {field}"
                );
                assert_eq!(receipt(&root), original);
            }
        }

        for graph in [false, true] {
            let page = if graph {
                orch.get_work_graph_page_scoped(&auth, session, &ws, Some(1), None, &[])
            } else {
                orch.list_work_page_scoped(&auth, session, &ws, Some(1), None, &[])
            }
            .unwrap();
            assert_eq!(page["complete"], true);
            assert_eq!(
                page[if graph { "nodes" } else { "items" }][0]["workId"],
                args["workId"]
            );
        }
        for path in paths(store.root()) {
            let bytes = std::fs::read(path).unwrap();
            let text = String::from_utf8(bytes).unwrap();
            assert!(!text.contains(TOKEN));
            if args["attemptId"].is_string() {
                assert!(
                    !text.contains(&token(&store, &args)),
                    "raw lease token persisted"
                );
            }
        }
    }
    assert!(
        paths(&store.root().join("runs")).len() == usize::from(op == "link"),
        "unexpected provider dispatch"
    );
    assert!(
        paths(&store.root().join("managed-intents")).is_empty(),
        "ordinary caller acquired managed effect authority"
    );
    assert!(host.shutdown().await.is_clean());
}
fn intents(root: &Path, op: &str) -> Vec<PathBuf> {
    paths(&root.join(if lifecycle(op) {
        "home/ledger/work-lifecycle-intents"
    } else {
        "home/ledger/work-intents"
    }))
}
fn original_response(intent: &Value, op: &str) -> Value {
    let item = intent["item"].clone();
    let scope = json!({"sessionId":intent["request"]["sessionId"],"workspace":intent["request"]["workspaceDisplay"]});
    match op {
        "claim" => {
            json!({"work":item,"attempt":WorkAttemptView::from(&serde_json::from_value::<WorkAttempt>(intent["attempts"][0].clone()).unwrap()),"sessionId":scope["sessionId"],"workspace":scope["workspace"]})
        }
        "renew" | "link" => {
            json!({"workId":item["workId"],"attempt":WorkAttemptView::from(&serde_json::from_value::<WorkAttempt>(intent["attempts"][0].clone()).unwrap()),"sessionId":scope["sessionId"],"workspace":scope["workspace"]})
        }
        "offer" | "reassign" | "authorize" => json!({"work":item,"decision":intent["decision"]}),
        "accept" | "decline" => {
            json!({"work":item,"decision":intent["decision"],"sessionId":scope["sessionId"],"workspace":scope["workspace"]})
        }
        "unblock" => {
            let mut work =
                json!({"workId":item["workId"],"revision":item["revision"],"state":item["state"]});
            if let Some(value) = item.get("blockProvenance") {
                work["blockProvenance"] = value.clone();
            }
            json!({"work":work,"sessionId":scope["sessionId"]})
        }
        "cancel" => {
            json!({"work":item,"attempts":intent["attempts"].as_array().unwrap().iter().map(|attempt|WorkAttemptView::from(&serde_json::from_value::<WorkAttempt>(attempt.clone()).unwrap())).collect::<Vec<_>>()})
        }
        "progress" | "release" | "complete" | "fail" | "approve" => {
            json!({"work":item,"attempt":WorkAttemptView::from(&serde_json::from_value::<WorkAttempt>(intent["attempts"][0].clone()).unwrap())})
        }
        _ => json!({"work":item,"sessionId":scope["sessionId"],"workspace":scope["workspace"]}),
    }
}
fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut result = paths(&root.join("home/ledger"))
        .into_iter()
        .filter(|path| {
            [
                "work-items",
                "work-attempts",
                "work-decisions",
                "work-intents",
                "work-lifecycle-intents",
            ]
            .iter()
            .any(|dir| path.components().any(|part| part.as_os_str() == *dir))
                || (path
                    .components()
                    .any(|part| part.as_os_str() == "idempotency")
                    && path.file_name().is_some_and(|name| {
                        name == format!("{}.json", safe_id_filename(REQUEST).unwrap()).as_str()
                    }))
        })
        .map(|path| {
            (
                path.strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let name = format!("{}.json", safe_id_filename(REQUEST).unwrap());
    for entry in walkdir::WalkDir::new(root.join("home/ledger/idempotency"))
        .into_iter()
        .filter_map(Result::ok)
    {
        if entry.file_type().is_symlink()
            && entry
                .path()
                .file_name()
                .is_some_and(|file| file == name.as_str())
        {
            result.insert(
                entry.path().strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read_link(entry.path())
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
            );
        }
    }
    if root.join("linked-original-receipt.json").exists() {
        result.insert(
            PathBuf::from("linked-original-receipt.json"),
            std::fs::read(root.join("linked-original-receipt.json")).unwrap(),
        );
    }
    result
}
fn prove(op: &str, stage: &str) {
    let root = tempfile::tempdir().unwrap();
    exited(&child(root.path(), op, "cut", stage), 86);
    let claim = receipt(root.path());
    let pending = intents(root.path(), op);
    let expected = if let Some(path) = pending.first() {
        original_response(&read(path), op)
    } else {
        claim["response"].clone()
    };
    assert!(!expected.is_null());
    let mut after = None;
    for phase in ["replay", "reopen2", "reopen3"] {
        exited(&child(root.path(), op, phase, ""), 0);
        let mut actual = read(root.path().join(format!("{phase}.json")))["response"].clone();
        if op == "claim" {
            let attempt: WorkAttempt = serde_json::from_value(read(
                paths(&root.path().join("home/ledger/work-attempts"))
                    .pop()
                    .unwrap(),
            ))
            .unwrap();
            assert_eq!(actual["leaseToken"], attempt.lease_token_for_secret(TOKEN));
            actual.as_object_mut().unwrap().remove("leaseToken");
        }
        assert_eq!(actual, expected, "{op}/{stage} original response");
        let settled = receipt(root.path());
        assert_eq!(settled["status"], "complete");
        assert_eq!(settled["response"], expected);
        for field in [
            "ownerId",
            "sessionId",
            "workspaceDigest",
            "tool",
            "requestId",
            "payloadHash",
        ] {
            assert_eq!(settled[field], claim[field]);
        }
        assert!(intents(root.path(), op).is_empty());
        assert_eq!(paths(&root.path().join("home/ledger/work-items")).len(), 1);
        let current = snapshot(root.path());
        if let Some(after) = &after {
            assert!(
                current == *after,
                "{op}/{stage} changed durable records after settled replay"
            );
        } else {
            after = Some(current);
        }
    }
}
#[test]
fn every_remaining_caller_replays_the_public_response_after_receipt_write() {
    for op in OPERATIONS {
        prove(
            op,
            if lifecycle(op) {
                "lifecycle_after_receipt"
            } else {
                "mutation_after_receipt"
            },
        );
    }
}

fn refuse_conflicting_receipt(op: &str) {
    for variant in [
        "missing",
        "payload",
        "response",
        "subject",
        "error",
        "failed",
        "linked",
        "owner",
        "session",
        "workspace",
        "tool",
        "malformed",
    ] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                op,
                "cut",
                if lifecycle(op) {
                    "lifecycle_after_intent"
                } else {
                    "mutation_after_intent"
                },
            ),
            86,
        );
        let path = receipt_path(root.path());
        let mut claim = read(&path);
        match variant {
            "missing" => std::fs::remove_file(&path).unwrap(),
            "payload" => {
                claim["payloadHash"] = json!("foreign payload");
                write(&path, &claim);
            }
            "response" => {
                claim["response"] = json!({"foreign":true});
                write(&path, &claim);
            }
            "subject" => {
                claim["runId"] = json!("foreign subject");
                write(&path, &claim);
            }
            "error" => {
                claim["error"] = json!({"code":"conflict","message":"foreign"});
                write(&path, &claim);
            }
            "failed" => {
                claim["status"] = json!("failed");
                write(&path, &claim);
            }
            "linked" => {
                let target = root.path().join("linked-original-receipt.json");
                std::fs::rename(&path, &target).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(target, &path).unwrap();
            }
            "owner" => {
                claim["ownerId"] = json!("foreign-owner");
                write(&path, &claim);
            }
            "session" => {
                claim["sessionId"] = json!(Uuid::new_v4());
                write(&path, &claim);
            }
            "workspace" => {
                claim["workspaceDigest"] = json!("foreign-workspace");
                write(&path, &claim);
            }
            "tool" => {
                claim["tool"] = json!("foreign-tool");
                write(&path, &claim);
            }
            "malformed" => std::fs::write(&path, b"{malformed-original-receipt").unwrap(),
            _ => unreachable!(),
        }
        let before = snapshot(root.path());
        for phase in ["refuse1", "refuse2", "refuse3"] {
            assert!(
                !child(root.path(), op, phase, "").status.success(),
                "{op}/{variant} accepted conflicting receipt"
            );
            assert!(
                snapshot(root.path()) == before,
                "{op}/{variant} recovery rewrote evidence before original receipt proof"
            );
        }
    }
}
#[test]
fn decision_caller_recovery_refuses_a_conflicting_original_receipt_before_writes() {
    refuse_conflicting_receipt("priority");
}
#[test]
fn lifecycle_caller_recovery_refuses_a_conflicting_original_receipt_before_writes() {
    refuse_conflicting_receipt("progress");
}

#[test]
fn remaining_production_callers_replay_each_admitted_process_cut() {
    for op in OPERATIONS {
        let stages = if lifecycle(op) {
            vec![
                "lifecycle_after_intent",
                "lifecycle_after_attempts",
                "lifecycle_after_item",
                "lifecycle_after_intent_remove",
                "lifecycle_after_commit",
            ]
        } else {
            vec![
                "mutation_after_intent",
                "mutation_after_decision",
                "mutation_after_item",
                "mutation_after_intent_remove",
                "mutation_after_commit",
            ]
        };
        for stage in stages {
            prove(op, stage);
        }
    }
}
#[test]
fn pre_intent_production_caller_cuts_preserve_the_prior_records() {
    for op in OPERATIONS {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                op,
                "cut",
                if lifecycle(op) {
                    "lifecycle_before_intent"
                } else {
                    "mutation_before_intent"
                },
            ),
            86,
        );
        assert_eq!(receipt(root.path())["status"], "pending");
        assert!(intents(root.path(), op).is_empty());
        let mut before = snapshot(root.path());
        before.retain(|path, _| {
            !path
                .components()
                .any(|part| part.as_os_str() == "idempotency")
        });
        let mut first = None;
        for _ in 0..3 {
            exited(&child(root.path(), op, "refusal", ""), 0);
            let claim = receipt(root.path());
            assert_eq!(claim["status"], "failed");
            let mut after = snapshot(root.path());
            after.retain(|path, _| {
                !path
                    .components()
                    .any(|part| part.as_os_str() == "idempotency")
            });
            assert!(
                after == before,
                "{op} pre-admission crash changed Work records"
            );
            if let Some(first) = &first {
                assert_eq!(&claim, first);
            } else {
                first = Some(claim);
            }
        }
    }
}

#[test]
fn work_caller_receipt_identifies_its_original_named_work() {
    for op in ["assign", "priority", "progress"] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                op,
                "cut",
                if lifecycle(op) {
                    "lifecycle_after_receipt"
                } else {
                    "mutation_after_receipt"
                },
            ),
            86,
        );
        let args = read(root.path().join("fixture.json"));
        assert_eq!(receipt(root.path())["runId"],args["workId"],"{op}: caller completes Some(work_id), so its sealed receipt must preserve that subject");
    }
}
#[test]
fn legacy_completed_work_receipts_without_subject_replay_without_rewriting() {
    for op in ["assign", "priority", "progress"] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                op,
                "cut",
                if lifecycle(op) {
                    "lifecycle_after_receipt"
                } else {
                    "mutation_after_receipt"
                },
            ),
            86,
        );
        let path = receipt_path(root.path());
        let mut claim = read(&path);
        claim["runId"] = Value::Null;
        write(&path, &claim);
        let bytes = std::fs::read(&path).unwrap();
        for phase in ["replay", "reopen2", "reopen3"] {
            exited(&child(root.path(), op, phase, ""), 0);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                read(root.path().join(format!("{phase}.json")))["response"],
                claim["response"]
            );
        }
    }
}

#[test]
fn all_remaining_callers_distinguish_pre_admission_refusal_from_live_recovery() {
    for op in OPERATIONS {
        let mut faults = vec!["before_intent", "item", "receipt", "cleanup"];
        // Renew and link change the attempt only; their Work snapshot is
        // intentionally unchanged and the committer performs no item write.
        if matches!(*op, "renew" | "link") {
            faults.retain(|fault| *fault != "item");
        }

        if !lifecycle(op) {
            faults.push("decision");
        }
        if matches!(
            *op,
            "claim"
                | "renew"
                | "link"
                | "progress"
                | "release"
                | "complete"
                | "fail"
                | "cancel"
                | "approve"
        ) {
            faults.push("attempt");
        }
        for fault in faults {
            let root = tempfile::tempdir().unwrap();
            exited(&child(root.path(), op, "fault", fault), 0);
            let claim = receipt(root.path());
            if fault == "before_intent" {
                assert_eq!(claim["status"], "failed");
                assert!(intents(root.path(), op).is_empty());
                let original = snapshot(root.path());
                for _ in 0..3 {
                    exited(&child(root.path(), op, "refusal", ""), 0);
                    assert!(snapshot(root.path()) == original);
                }
            } else {
                assert_eq!(
                    claim["status"],
                    if fault == "cleanup" {
                        "complete"
                    } else {
                        "pending"
                    }
                );
                let expected =
                    original_response(&read(intents(root.path(), op).pop().unwrap()), op);
                let mut after = None;
                for phase in ["replay", "reopen2", "reopen3"] {
                    exited(&child(root.path(), op, phase, ""), 0);
                    let mut response =
                        read(root.path().join(format!("{phase}.json")))["response"].clone();
                    if *op == "claim" {
                        response.as_object_mut().unwrap().remove("leaseToken");
                    }
                    assert_eq!(response, expected);
                    let current = snapshot(root.path());
                    if let Some(after) = &after {
                        assert!(current == *after);
                    } else {
                        after = Some(current);
                    }
                }
            }
        }
    }
}
