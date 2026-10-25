//! Actual authenticated manager mutation callers, process death and replay.
mod common;
#[path = "common/copy_tree.rs"]
mod copy_tree;

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, safe_id_filename, AuthContext, ManagerCoordinationMode, ManagerStepSpec,
    MessageKind, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkItem,
    WorkMessage, WorkPolicy, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, DurableWriteFault, HostConfig,
    McpControlClient, SessionKind, DURABLE_WRITE_FAULT, PRUNE_REMOVE_FAULT,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::Ordering;
use uuid::Uuid;

const TOKEN: &str = "synthetic-manager-mutation-v2";
const REQUEST: &str = "original-manager-mutation";
fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut result: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|ext| ext == "json")
        })
        .map(|entry| entry.into_path())
        .collect();
    result.sort();
    result
}
fn receipt(root: &Path) -> Value {
    paths(&root.join("home/ledger/idempotency"))
        .into_iter()
        .map(read)
        .find(|value| value["requestId"] == REQUEST)
        .unwrap()
}
fn child(root: &Path, operation: &str, phase: &str, stage: &str) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "manager_mutation_child", "--nocapture"])
        .env("MANAGER_MUTATION_ROOT", root)
        .env("MANAGER_MUTATION_OP", operation)
        .env("MANAGER_MUTATION_PHASE", phase)
        .env("MANAGER_MUTATION_STAGE", stage)
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
fn specs(replacement: bool) -> Vec<ManagerStepSpec> {
    serde_json::from_value(if replacement {
        json!([
            {"stepId":"replacement-a","kind":"verification","objective":"replacement first step"},
            {"stepId":"replacement-b","kind":"verification","objective":"replacement second step"}
        ])
    } else {
        json!([
            {"stepId":"first","kind":"verification","objective":"first synthetic step"},
            {"stepId":"second","kind":"verification","objective":"second synthetic step"}
        ])
    })
    .unwrap()
}

async fn invoke(
    orch: &OrchestrationService,
    auth: &AuthContext,
    args: &Value,
    operation: &str,
) -> Result<Value, grokptah_agent_bridge::orchestration::OrchError> {
    let session = Uuid::parse_str(args["sessionId"].as_str().unwrap()).unwrap();
    let workspace = Path::new(args["workspace"].as_str().unwrap());
    let plan = args["planId"].as_str().unwrap();
    let revision = args["expectedRevision"].as_u64();
    match operation {
        "advance" => {
            orch.advance_manager_plan(auth, REQUEST, session, workspace, plan, revision)
                .await
        }
        "tick" | "tick-message" | "tick-question-reuse" => {
            orch.tick_manager_plan(auth, REQUEST, session, workspace, plan, revision)
                .await
        }
        "replan" => {
            orch.replan_manager_plan(
                auth,
                REQUEST,
                session,
                workspace,
                plan,
                "replace the cancelled step".into(),
                specs(true),
                revision,
            )
            .await
        }
        other => panic!("unknown manager operation {other}"),
    }
}

#[test]
fn manager_mutation_child() {
    let Ok(root) = std::env::var("MANAGER_MUTATION_ROOT") else {
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
    let operation = std::env::var("MANAGER_MUTATION_OP").unwrap();
    let phase = std::env::var("MANAGER_MUTATION_PHASE").unwrap();
    std::fs::create_dir_all(root.join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.join("workspace")).unwrap();
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
    host.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.join("home/ledger")).unwrap();
    // Recovery itself runs on open. A fresh supervisor is entitled to make
    // further progress (including decision Work) after this checkpoint; test
    // the recovered background transaction before starting another writer.
    if operation.starts_with("background") && !setup {
        let args = read(root.join("fixture.json"));
        write(
            root.join(format!("{phase}.json")),
            &json!({"response":{"plan":store.load_manager_plan(args["planId"].as_str().unwrap()).unwrap().unwrap()}}),
        );
        assert!(host.shutdown().await.is_clean());
        return;
    }
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    if operation.starts_with("background") {
        assert!(orch.stop_background_tasks().await.fully_stopped);
    }
    let args = if setup {
        let agent = host.ensure_session_agent(session).unwrap();
        let created = orch
            .create_manager_plan(
                &auth,
                "setup-manager",
                session,
                &workspace,
                agent.agent_id,
                "manager replay fixture".into(),
                specs(false),
                2,
                2,
                false,
            )
            .await
            .unwrap();
        let id = created["plan"]["planId"].as_str().unwrap();
        let plan = if operation == "replan"
            || operation == "tick-message"
            || operation == "background-message"
            || operation == "tick-question-reuse"
        {
            let initial = orch
                .advance_manager_plan(&auth, "setup-advance", session, &workspace, id, Some(1))
                .await
                .unwrap();
            let work_id = initial["createdWork"][0]["workId"].as_str().unwrap();
            if operation != "tick-question-reuse" {
                orch.cancel_work(
                    &auth,
                    "setup-cancel",
                    session,
                    &workspace,
                    work_id,
                    "synthetic cancelled step".into(),
                    None,
                )
                .await
                .unwrap();
            }
            if operation == "replan" {
                orch.advance_manager_plan(
                    &auth,
                    "setup-observe",
                    session,
                    &workspace,
                    id,
                    Some(initial["plan"]["revision"].as_u64().unwrap()),
                )
                .await
                .unwrap()["plan"]
                    .clone()
            } else if operation == "tick-question-reuse" {
                let mut work = store.load_work_item(work_id).unwrap().unwrap();
                work.state = grokptah_agent_bridge::orchestration::WorkState::AwaitingInput;
                work.blocked_reason = Some("synthetic question".into());
                work.bump_at(chrono::Utc::now());
                store.save_work_item(&work).unwrap();
                let observed = orch
                    .tick_manager_plan(
                        &auth,
                        "setup-first-question",
                        session,
                        &workspace,
                        id,
                        Some(initial["plan"]["revision"].as_u64().unwrap()),
                    )
                    .await
                    .unwrap();
                let mut plan = store.load_manager_plan(id).unwrap().unwrap();
                plan.steps[0].last_notification_work_revision = None;
                plan.steps[0].last_notification_message_id = None;
                plan.revision += 1;
                plan.updated_at = chrono::Utc::now();
                store.save_manager_plan(&plan).unwrap();
                write(
                    root.join("original-question.json"),
                    &observed["messages"][0],
                );
                json!(plan)
            } else {
                initial["plan"].clone()
            }
        } else {
            created["plan"].clone()
        };
        let args = json!({"sessionId":session,"workspace":workspace,"planId":id,"expectedRevision":plan["revision"]});
        write(root.join("fixture.json"), &args);
        write(root.join("prior-plan.json"), &plan);
        let options = if root.join("options.json").exists() {
            read(root.join("options.json"))
        } else {
            json!({})
        };
        for index in 0..options["seedCount"].as_u64().unwrap_or(0) {
            let mut work = WorkItem::new(
                "test",
                "synthetic manager capacity",
                session,
                workspace.display().to_string(),
                "synthetic-fixture",
                WorkPolicy::default(),
            )
            .unwrap();
            work.work_id = format!("manager-capacity-{index:04}");
            write(
                store.root().join(format!(
                    "work-items/{}.json",
                    safe_id_filename(&work.work_id).unwrap()
                )),
                &json!(work),
            );
        }

        for index in 1..=options["seedMessages"].as_u64().unwrap_or(0) {
            let mut message = WorkMessage::new(
                MessageKind::Status,
                "synthetic-seed",
                None,
                None,
                session,
                workspace.display().to_string(),
                None,
                "synthetic retention fixture",
                None,
                chrono::Utc::now(),
            )
            .unwrap();
            message.message_id = format!("retained-seed-{index:04}");
            message.seq = index;
            write(
                store.root().join(format!(
                    "messages/{}.json",
                    safe_id_filename(&message.message_id).unwrap()
                )),
                &json!(message),
            );
        }
        if let Some(count) = options["seedMessages"].as_u64() {
            std::fs::write(store.root().join("messages/seq"), count.to_string()).unwrap();
        }
        args
    } else {
        read(root.join("fixture.json"))
    };
    if setup && operation.starts_with("background") {
        let mut plan = store
            .load_manager_plan(args["planId"].as_str().unwrap())
            .unwrap()
            .unwrap();
        plan.coordination.mode = ManagerCoordinationMode::Autonomous;
        store.save_manager_plan(&plan).unwrap();
        write(root.join("prior-plan.json"), &json!(plan));
    }
    if phase == "cut" {
        env.set(
            "GROKPTAH_CRASH_STAGE",
            std::env::var("MANAGER_MUTATION_STAGE").unwrap(),
        );
    }
    if phase == "fault" {
        let stage = std::env::var("MANAGER_MUTATION_STAGE").unwrap();
        let (needle, skips_remaining) = match stage.as_str() {
            "before_intent" => ("manager-intents", 0),
            "first_work" => ("work-items", 0),
            "second_work" => ("work-items", 1),
            "plan" => ("manager-plans", 0),
            "sequence" => ("messages/seq", 0),
            "message" => ("messages/", 1),
            "receipt" => ("idempotency/v2", 0),
            "cleanup" => ("manager-intents", 1),
            _ => panic!("unknown fault {stage}"),
        };
        *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
            needle,
            skips_remaining,
        });
    }
    let outcome = if operation.starts_with("background") {
        if phase == "cut" {
            orch.drive_manager_supervisor_once().await;
            assert!(orch.manager_supervisor_status().last_error.is_none());
        }
        json!({"response":{"plan":store.load_manager_plan(args["planId"].as_str().unwrap()).unwrap().unwrap()}})
    } else {
        if phase == "prune_remove_fault" {
            PRUNE_REMOVE_FAULT.store(true, Ordering::SeqCst);
        }
        match invoke(&orch, &auth, &args, &operation).await {
            Ok(response) => json!({"response":response}),
            Err(error) => json!({"error":error}),
        }
    };
    if phase == "prune_remove_fault" {
        PRUNE_REMOVE_FAULT.store(false, Ordering::SeqCst);
    }
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
    write(root.join(format!("{phase}.json")), &outcome);
    if phase == "fault" || phase == "refusal" {
        assert!(
            outcome.get("error").is_some(),
            "expected refusal: {outcome}"
        );
        if phase == "fault" && std::env::var("MANAGER_MUTATION_STAGE").unwrap() != "before_intent" {
            assert!(store.ledger_admission_closed().is_some());
            assert!(store
                .send_message(
                    WorkMessage::new(
                        MessageKind::Status,
                        "synthetic-later",
                        None,
                        None,
                        session,
                        workspace.display().to_string(),
                        None,
                        "must not write after uncertainty",
                        None,
                        chrono::Utc::now()
                    )
                    .unwrap()
                )
                .is_err());
            if let Some(path) = paths(&store.root().join("messages")).first() {
                assert!(store
                    .ack_message(
                        read(path)["messageId"].as_str().unwrap(),
                        "synthetic-later",
                        chrono::Utc::now()
                    )
                    .is_err());
                assert!(store
                    .ack_message_scoped(
                        read(path)["messageId"].as_str().unwrap(),
                        "synthetic-later",
                        chrono::Utc::now(),
                        session,
                        &workspace.display().to_string()
                    )
                    .is_err());
            }
            assert!(store
                .save_manager_plan(
                    &store
                        .load_manager_plan(args["planId"].as_str().unwrap())
                        .unwrap()
                        .unwrap()
                )
                .is_err());
        }
    } else if phase != "prune_remove_fault" {
        assert!(
            outcome.get("response").is_some(),
            "caller failed: {outcome}; durable receipt: {}",
            receipt(&root)
        );
        if !operation.starts_with("background") {
            let original = receipt(&root);
            let mut changed = args.clone();
            changed["expectedRevision"] = json!(args["expectedRevision"].as_u64().unwrap() + 1);
            assert!(invoke(&orch, &auth, &changed, &operation).await.is_err());
            assert_eq!(receipt(&root), original);
        }
        if phase == "mutate" {
            let current = store
                .load_manager_plan(args["planId"].as_str().unwrap())
                .unwrap()
                .unwrap();
            let child_id = current
                .steps
                .iter()
                .find_map(|step| step.work_id.as_ref())
                .unwrap();
            orch.cancel_work(
                &auth,
                "later-manager-child-cancel",
                session,
                &workspace,
                child_id,
                "newer independent outcome".into(),
                None,
            )
            .await
            .unwrap();
            let later = orch
                .advance_manager_plan(
                    &auth,
                    "later-manager-observation",
                    session,
                    &workspace,
                    &current.plan_id,
                    Some(current.revision),
                )
                .await
                .unwrap();
            assert_ne!(later["plan"], outcome["response"]["plan"]);
            write(root.join("later-plan.json"), &later["plan"]);
            assert_eq!(receipt(&root)["response"], outcome["response"]);
        }
        let expected = store
            .list_work_items()
            .unwrap()
            .into_iter()
            .map(|work| work.work_id)
            .collect::<std::collections::BTreeSet<_>>();
        let options = if root.join("options.json").exists() {
            read(root.join("options.json"))
        } else {
            json!({})
        };
        let transport = if options["mcpRead"] == true && phase == "replay" {
            let server = start_control_server(orch.clone(), 0).await.unwrap();
            let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
            client.initialize().await.unwrap();
            Some((server, client))
        } else {
            None
        };
        let mut transport = transport;
        for graph in [false, true] {
            if expected.len() > 200 && phase != "replay" {
                continue;
            }
            let mut cursor = None;
            let mut ids = Vec::new();
            let mut complete = false;
            for _ in 0..expected.len() + 1 {
                let page = if let Some((_, client)) = &mut transport {
                    let mut input = json!({"session_id":session,"workspace":workspace,"limit":200});
                    if let Some(cursor) = &cursor {
                        input["cursor"] = json!(cursor);
                    }
                    let response = client
                        .call_tool(
                            if graph {
                                "ptah_get_work_graph_page"
                            } else {
                                "ptah_list_work_page"
                            },
                            input,
                        )
                        .await
                        .unwrap();
                    assert!(!response.is_error);
                    response.structured
                } else {
                    if graph {
                        orch.get_work_graph_page_scoped(
                            &auth,
                            session,
                            &workspace,
                            Some(if expected.len() > 200 { 200 } else { 1 }),
                            cursor.clone(),
                            &[],
                        )
                    } else {
                        orch.list_work_page_scoped(
                            &auth,
                            session,
                            &workspace,
                            Some(if expected.len() > 200 { 200 } else { 1 }),
                            cursor.clone(),
                            &[],
                        )
                    }
                    .unwrap()
                };
                ids.extend(
                    page[if graph { "nodes" } else { "items" }]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["workId"].as_str().unwrap().to_string()),
                );
                if page["complete"] == true {
                    complete = true;
                    break;
                }
                let next = page["continuation"].as_str().unwrap().to_string();
                assert_ne!(cursor.as_ref(), Some(&next));
                cursor = Some(next);
            }
            assert!(complete);
            assert_eq!(ids.len(), expected.len());
            assert_eq!(
                ids.into_iter().collect::<std::collections::BTreeSet<_>>(),
                expected
            );
        }
        if let Some((server, mut client)) = transport {
            let work_id = outcome["response"]["createdWork"][0]["workId"]
                .as_str()
                .unwrap();
            let detail=client.call_tool("ptah_get_work_detail",json!({"session_id":session,"workspace":workspace,"work_id":work_id,"section":"summary"})).await.unwrap();
            assert!(!detail.is_error);
            assert_eq!(detail.structured["body"]["workId"], work_id);
            assert!(server.stop_and_wait().await.fully_stopped);
        }
    }
    assert!(orch.stop_background_tasks().await.fully_stopped);
    assert!(host.shutdown().await.is_clean());
}
fn sealed_response(intent: &Value) -> Value {
    let mut result = json!({"plan":intent["plan"]});
    if intent["operation"] != "replan" {
        result["createdWork"] = json!(intent["workWrites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|write| write["item"].clone())
            .collect::<Vec<_>>());
    }
    if intent["operation"] == "tick" {
        result["messages"] = json!(intent["messageWrites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|write| write["message"].clone())
            .collect::<Vec<_>>());
        result["nativeExecutor"] = intent["nativeExecutor"].clone();
    }
    result
}
fn prove(operation: &str, stage: &str) {
    let root = tempfile::tempdir().unwrap();
    exited(&child(root.path(), operation, "cut", stage), 86);
    let original = receipt(root.path());
    assert_eq!(
        original["status"],
        if matches!(
            stage,
            "manager_mutation_after_receipt"
                | "manager_mutation_after_intent_remove"
                | "manager_mutation_after_commit"
        ) {
            "complete"
        } else {
            "pending"
        }
    );
    let intents = paths(&root.path().join("home/ledger/manager-intents"));
    let expected = if let Some(path) = intents.first() {
        sealed_response(&read(path))
    } else {
        original["response"].clone()
    };
    assert!(!expected.is_null());
    let mut expected_ids = paths(&root.path().join("home/ledger/work-items"))
        .into_iter()
        .map(|path| read(path)["workId"].as_str().unwrap().to_string())
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(items) = expected["createdWork"].as_array() {
        expected_ids.extend(
            items
                .iter()
                .map(|item| item["workId"].as_str().unwrap().to_string()),
        );
    }
    let root_id = expected["plan"]["rootWorkId"].as_str().unwrap();
    let root_work = read(root.path().join(format!(
        "home/ledger/work-items/{}.json",
        safe_id_filename(root_id).unwrap()
    )));
    let mut durable = None;
    for phase in ["replay", "reopen2", "reopen3"] {
        exited(&child(root.path(), operation, phase, ""), 0);
        let actual = read(root.path().join(format!("{phase}.json")))["response"].clone();
        assert_eq!(actual, expected);
        let settled = receipt(root.path());
        assert_eq!(settled["status"], "complete");
        assert_eq!(settled["response"], expected);
        assert_eq!(settled["runId"], expected["plan"]["planId"]);
        for field in [
            "ownerId",
            "sessionId",
            "workspaceDigest",
            "payloadHash",
            "tool",
            "requestId",
        ] {
            assert_eq!(settled[field], original[field]);
        }
        let ids = paths(&root.path().join("home/ledger/work-items"))
            .into_iter()
            .map(|path| read(path)["workId"].as_str().unwrap().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids, expected_ids);
        assert_eq!(
            read(root.path().join(format!(
                "home/ledger/work-items/{}.json",
                safe_id_filename(root_id).unwrap()
            ))),
            root_work
        );
        if let Some(messages) = expected["messages"].as_array() {
            assert_eq!(
                paths(&root.path().join("home/ledger/messages")).len(),
                messages.len()
            );
            for message in messages {
                assert_eq!(
                    read(root.path().join(format!(
                        "home/ledger/messages/{}.json",
                        safe_id_filename(message["messageId"].as_str().unwrap()).unwrap()
                    ))),
                    *message
                );
            }
        }
        assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
        let snapshot = disk_json(root.path());
        if let Some(prior) = &durable {
            assert_eq!(&snapshot, prior);
        } else {
            durable = Some(snapshot);
        }
    }
}
fn disk_json(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
    let mut result = paths(&root.join("home/ledger"))
        .into_iter()
        .map(|path| {
            (
                path.strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    // Audit/status and host reconciliation metadata can legitimately change;
    // compare records this operation owns and its original claim only.
    result.retain(|path, _| {
        [
            "work-items",
            "manager-plans",
            "messages",
            "manager-intents",
            "work-attempts",
            "work-decisions",
        ]
        .iter()
        .any(|dir| path.components().any(|part| part.as_os_str() == *dir))
            || (path
                .components()
                .any(|part| part.as_os_str() == "idempotency")
                && read(root.join(path))["requestId"] == REQUEST)
    });
    let sequence = root.join("home/ledger/messages/seq");
    if sequence.exists() {
        result.insert(
            PathBuf::from("home/ledger/messages/seq"),
            std::fs::read(sequence).unwrap(),
        );
    }
    if root.join("linked-claim.json").exists() {
        result.insert(
            PathBuf::from("linked-claim.json"),
            std::fs::read(root.join("linked-claim.json")).unwrap(),
        );
    }
    result
}
#[test]
fn advance_child_written_cut_replays_the_original_request() {
    prove("advance", "manager_mutation_after_work");
}
#[test]
fn tick_child_written_cut_replays_the_original_request() {
    prove("tick", "manager_mutation_after_work");
}
#[test]
fn replan_written_cut_replays_the_original_request() {
    prove("replan", "manager_mutation_after_plan");
}
#[test]
fn tick_notification_written_cut_replays_the_original_request() {
    prove("tick-message", "manager_mutation_after_plan");
}
#[test]
fn manager_mutation_cut_matrix_preserves_sealed_responses() {
    for operation in ["advance", "tick", "replan", "tick-message"] {
        for stage in [
            "manager_mutation_after_intent",
            "manager_mutation_after_first_work",
            "manager_mutation_after_work",
            "manager_mutation_after_sequence",
            "manager_mutation_after_first_message",
            "manager_mutation_after_messages",
            "manager_mutation_after_plan",
            "manager_mutation_after_receipt",
            "manager_mutation_after_intent_remove",
            "manager_mutation_after_commit",
        ] {
            if stage == "manager_mutation_after_first_work"
                && matches!(operation, "replan" | "tick-message")
            {
                continue;
            }
            if matches!(
                stage,
                "manager_mutation_after_sequence" | "manager_mutation_after_first_message"
            ) && operation != "tick-message"
            {
                continue;
            }
            prove(operation, stage);
        }
    }
}
#[test]
fn manager_mutation_recovery_refuses_conflicting_evidence_without_rewriting_records() {
    for operation in ["advance", "replan", "tick-message"] {
        for variant in [
            "plan",
            "work",
            "message",
            "sequence",
            "receipt_payload",
            "receipt_response",
            "receipt_subject",
            "receipt_error",
            "missing_receipt",
            "receipt_link",
            "intent_digest",
            "intent_filename",
            "intent_request",
            "intent_schema",
        ] {
            if matches!(variant, "message" | "sequence") && operation != "tick-message" {
                continue;
            }
            if variant == "work" && operation != "advance" {
                continue;
            }
            let root = tempfile::tempdir().unwrap();
            exited(
                &child(root.path(), operation, "cut", "manager_mutation_after_plan"),
                86,
            );
            let ledger = root.path().join("home/ledger");
            let intent_path = paths(&ledger.join("manager-intents")).pop().unwrap();
            let mut intent = read(&intent_path);
            let receipt_path = paths(&ledger.join("idempotency"))
                .into_iter()
                .find(|path| read(path)["requestId"] == REQUEST)
                .unwrap();
            let mut claim = read(&receipt_path);
            match variant {
                "plan" => {
                    let path = ledger.join(format!(
                        "manager-plans/{}.json",
                        safe_id_filename(intent["plan"]["planId"].as_str().unwrap()).unwrap()
                    ));
                    let mut value = read(&path);
                    value["objective"] = json!("foreign plan");
                    write(path, &value);
                }
                "work" => {
                    let path = ledger.join(format!(
                        "work-items/{}.json",
                        safe_id_filename(
                            intent["workWrites"][0]["item"]["workId"].as_str().unwrap()
                        )
                        .unwrap()
                    ));
                    let mut value = read(&path);
                    value["objective"] = json!("foreign work");
                    write(path, &value);
                }
                "message" => {
                    let path = ledger.join(format!(
                        "messages/{}.json",
                        safe_id_filename(
                            intent["messageWrites"][0]["message"]["messageId"]
                                .as_str()
                                .unwrap()
                        )
                        .unwrap()
                    ));
                    let mut value = read(&path);
                    value["body"] = json!("foreign notification");
                    write(path, &value);
                }
                "sequence" => std::fs::write(ledger.join("messages/seq"), "999999").unwrap(),
                "receipt_payload" => {
                    claim["payloadHash"] = json!("foreign hash");
                    write(&receipt_path, &claim);
                }
                "receipt_response" => {
                    claim["response"] = json!({"foreign":true});
                    write(&receipt_path, &claim);
                }
                "receipt_subject" => {
                    claim["runId"] = json!("foreign subject");
                    write(&receipt_path, &claim);
                }
                "receipt_error" => {
                    claim["error"] = json!({"code":"conflict","message":"foreign"});
                    write(&receipt_path, &claim);
                }
                "missing_receipt" => std::fs::remove_file(&receipt_path).unwrap(),
                "receipt_link" => {
                    let target = root.path().join("linked-claim.json");
                    std::fs::rename(&receipt_path, &target).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&target, &receipt_path).unwrap();
                }
                "intent_digest" => {
                    intent["snapshotDigest"] = json!("foreign digest");
                    write(&intent_path, &intent);
                }
                "intent_filename" => {
                    std::fs::rename(&intent_path, ledger.join("manager-intents/foreign.json"))
                        .unwrap()
                }
                "intent_request" => {
                    intent["request"]["binding"]["requestId"] = json!("foreign original request");
                    write(&intent_path, &intent);
                }
                "intent_schema" => {
                    intent["schemaVersion"] = json!(4);
                    write(&intent_path, &intent);
                }
                _ => unreachable!(),
            }
            let before = disk_json(root.path());
            for phase in ["refuse1", "refuse2", "refuse3"] {
                let result = child(root.path(), operation, phase, "");
                assert!(
                    !result.status.success(),
                    "unexpected recovery for {operation}/{variant}"
                );
                assert_eq!(
                    disk_json(root.path()),
                    before,
                    "records changed for {operation}/{variant}"
                );
            }
        }
    }
}

#[test]
fn manager_mutation_write_faults_preserve_original_claim_and_close_admission() {
    for operation in ["advance", "tick", "replan", "tick-message"] {
        for fault in [
            "before_intent",
            "first_work",
            "second_work",
            "sequence",
            "message",
            "plan",
            "receipt",
            "cleanup",
        ] {
            if matches!(fault, "first_work" | "second_work")
                && matches!(operation, "replan" | "tick-message")
            {
                continue;
            }
            if matches!(fault, "sequence" | "message") && operation != "tick-message" {
                continue;
            }
            let root = tempfile::tempdir().unwrap();
            exited(&child(root.path(), operation, "fault", fault), 0);
            let original = receipt(root.path());
            if fault == "before_intent" {
                assert_eq!(original["status"], "failed");
                assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
                for phase in ["refusal", "refusal", "refusal"] {
                    exited(&child(root.path(), operation, phase, ""), 0);
                    assert_eq!(receipt(root.path()), original);
                }
            } else {
                assert_eq!(
                    original["status"],
                    if fault == "cleanup" {
                        "complete"
                    } else {
                        "pending"
                    }
                );
                let expected = sealed_response(&read(
                    paths(&root.path().join("home/ledger/manager-intents"))
                        .pop()
                        .unwrap(),
                ));
                for phase in ["replay", "reopen2", "reopen3"] {
                    exited(&child(root.path(), operation, phase, ""), 0);
                    assert_eq!(
                        read(root.path().join(format!("{phase}.json")))["response"],
                        expected
                    );
                    assert_eq!(receipt(root.path())["response"], expected);
                }
            }
        }
    }
}

#[test]
fn recomputed_manager_seals_cannot_substitute_an_original_derivation() {
    for (operation, variant) in [
        ("advance", "child_objective"),
        ("advance", "child_order"),
        ("advance", "plan_state"),
        ("advance", "new_child_prior"),
        ("tick-message", "message_body"),
        ("tick-message", "message_identity"),
        ("tick-message", "notification_work"),
        ("tick-message", "observation_clock"),
        ("tick-message", "observation_actor"),
        ("replan", "new_reason"),
    ] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                operation,
                "cut",
                "manager_mutation_after_intent",
            ),
            86,
        );
        let path = paths(&root.path().join("home/ledger/manager-intents"))
            .pop()
            .unwrap();
        let mut intent = read(&path);
        match variant {
            "child_order" => intent["workWrites"].as_array_mut().unwrap().reverse(),
            "child_objective" => {
                intent["workWrites"][0]["item"]["objective"] = json!("substituted child")
            }
            "plan_state" => intent["plan"]["state"] = json!("succeeded"),
            "new_child_prior" => {
                intent["workWrites"][0]["prior"] = intent["workWrites"][0]["item"].clone()
            }
            "message_body" => {
                intent["messageWrites"][0]["message"]["body"] = json!("substituted notification")
            }
            "message_identity" => {
                intent["messageWrites"][0]["message"]["messageId"] = json!("foreign-notification");
                intent["plan"]["steps"][0]["lastNotificationMessageId"] =
                    json!("foreign-notification");
            }
            "notification_work" => {
                intent["observation"]["workItems"][0]["result"] =
                    json!({"failure":"foreign outcome"})
            }
            "observation_clock" => {
                intent["observation"]["notifiedAt"] = json!("2020-01-01T00:00:00Z")
            }
            "observation_actor" => intent["observation"]["actor"] = json!("foreign-actor"),
            "new_reason" => {
                intent["request"]["payload"]["reason"] = json!("substituted reason");
                intent["request"]["payloadDigest"] =
                    json!(grokptah_agent_bridge::orchestration::hash_payload(
                        &intent["request"]["payload"]
                    ));
            }
            _ => unreachable!(),
        }
        intent["snapshotDigest"] = json!(grokptah_agent_bridge::orchestration::hash_payload(
            &json!([
                intent["operation"],
                intent["priorPlan"],
                intent["plan"],
                intent["workWrites"],
                intent["messageWrites"],
                intent["sequence"],
                intent["nativeExecutor"],
                intent["observation"]
            ])
        ));
        write(&path, &intent);
        let before = disk_json(root.path());
        for phase in ["refuse1", "refuse2", "refuse3"] {
            assert!(
                !child(root.path(), operation, phase, "").status.success(),
                "accepted {variant}"
            );
            assert_eq!(disk_json(root.path()), before);
        }
    }
}

#[test]
fn background_manager_materialization_has_explicitly_no_foreground_receipt() {
    for operation in ["background", "background-message"] {
        for stage in [
            "manager_mutation_after_intent",
            "manager_mutation_after_work",
            "manager_mutation_after_plan",
            "manager_mutation_after_receipt",
        ] {
            let root = tempfile::tempdir().unwrap();
            exited(&child(root.path(), operation, "cut", stage), 86);
            let intent = read(
                paths(&root.path().join("home/ledger/manager-intents"))
                    .pop()
                    .unwrap(),
            );
            assert!(intent.get("request").unwrap().is_null());
            assert!(intent["nativeExecutor"].is_null());
            let mut prior = None;
            for phase in ["verify1", "verify2", "verify3"] {
                exited(&child(root.path(), operation, phase, ""), 0);
                assert_eq!(
                    read(root.path().join(format!("{phase}.json")))["response"]["plan"],
                    intent["plan"]
                );
                for write in intent["workWrites"].as_array().unwrap() {
                    assert_eq!(
                        read(root.path().join(format!(
                            "home/ledger/work-items/{}.json",
                            safe_id_filename(write["item"]["workId"].as_str().unwrap()).unwrap()
                        ))),
                        write["item"]
                    );
                }
                for write in intent["messageWrites"].as_array().unwrap() {
                    assert_eq!(
                        read(root.path().join(format!(
                                "home/ledger/messages/{}.json",
                                safe_id_filename(write["message"]["messageId"].as_str().unwrap())
                                    .unwrap()
                            ))),
                        write["message"]
                    );
                }
                assert!(!paths(&root.path().join("home/ledger/idempotency"))
                    .iter()
                    .any(|path| read(path)["requestId"] == REQUEST));
                let current = disk_json(root.path());
                if let Some(prior) = &prior {
                    assert_eq!(&current, prior);
                } else {
                    prior = Some(current);
                }
            }
        }
    }
}

#[test]
fn original_manager_response_stays_original_after_a_newer_plan_observation() {
    for operation in ["advance", "tick"] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                operation,
                "cut",
                "manager_mutation_after_receipt",
            ),
            86,
        );
        let expected = receipt(root.path())["response"].clone();
        exited(&child(root.path(), operation, "mutate", ""), 0);
        let later = disk_json(root.path());
        for phase in ["replay", "reopen2", "reopen3"] {
            exited(&child(root.path(), operation, phase, ""), 0);
            assert_eq!(
                read(root.path().join(format!("{phase}.json")))["response"],
                expected
            );
            assert_eq!(disk_json(root.path()), later);
        }
    }
}
#[test]
fn manager_child_batch_capacity_is_admitted_before_any_effect() {
    // Root plus 4093 seed records leaves exactly two child slots. One more
    // seed refuses the whole two-child batch without changing the plan.
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedCount":4093,"mcpRead":true}),
    );
    exited(
        &child(
            root.path(),
            "advance",
            "cut",
            "manager_mutation_after_first_work",
        ),
        86,
    );
    let intent = read(
        paths(&root.path().join("home/ledger/manager-intents"))
            .pop()
            .unwrap(),
    );
    let expected = sealed_response(&intent);
    for phase in ["replay", "reopen2", "reopen3"] {
        exited(&child(root.path(), "advance", phase, ""), 0);
        assert_eq!(
            read(root.path().join(format!("{phase}.json")))["response"],
            expected
        );
        assert_eq!(
            paths(&root.path().join("home/ledger/work-items")).len(),
            4096
        );
    }
    let overflow = tempfile::tempdir().unwrap();
    write(
        overflow.path().join("options.json"),
        &json!({"seedCount":4094}),
    );
    exited(&child(overflow.path(), "advance", "refusal", ""), 0);
    assert_eq!(
        paths(&overflow.path().join("home/ledger/work-items")).len(),
        4095
    );
    assert!(paths(&overflow.path().join("home/ledger/manager-intents")).is_empty());
    assert_eq!(
        read(
            paths(&overflow.path().join("home/ledger/manager-plans"))
                .pop()
                .unwrap()
        ),
        read(overflow.path().join("prior-plan.json"))
    );
    let original = receipt(overflow.path());
    assert_eq!(original["status"], "failed");
    for _ in 0..3 {
        exited(&child(overflow.path(), "advance", "refusal", ""), 0);
        assert_eq!(receipt(overflow.path()), original);
    }
}

#[test]
fn recovered_tick_preserves_retention_and_existing_question_identity() {
    for stage in [
        "manager_mutation_after_sequence",
        "manager_mutation_after_first_message",
        "manager_mutation_after_intent_remove",
    ] {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path().join("options.json"),
            &json!({"seedMessages":500}),
        );
        exited(&child(root.path(), "tick-message", "cut", stage), 86);
        let expected = paths(&root.path().join("home/ledger/manager-intents"))
            .first()
            .map(|path| sealed_response(&read(path)))
            .unwrap_or_else(|| receipt(root.path())["response"].clone());
        for phase in ["replay", "reopen2", "reopen3"] {
            exited(&child(root.path(), "tick-message", phase, ""), 0);
            assert_eq!(
                read(root.path().join(format!("{phase}.json")))["response"],
                expected
            );
            assert_eq!(paths(&root.path().join("home/ledger/messages")).len(), 500);
            assert_eq!(
                std::fs::read_to_string(root.path().join("home/ledger/messages/seq")).unwrap(),
                "501"
            );
        }
    }
    let root = tempfile::tempdir().unwrap();
    exited(
        &child(
            root.path(),
            "tick-question-reuse",
            "cut",
            "manager_mutation_after_messages",
        ),
        86,
    );
    let original = read(root.path().join("original-question.json"));
    let intent = read(
        paths(&root.path().join("home/ledger/manager-intents"))
            .pop()
            .unwrap(),
    );
    assert_eq!(intent["messageWrites"][0]["prior"], original);
    assert_eq!(intent["messageWrites"][0]["message"], original);
    let expected = sealed_response(&intent);
    for phase in ["replay", "reopen2", "reopen3"] {
        exited(&child(root.path(), "tick-question-reuse", phase, ""), 0);
        assert_eq!(
            read(root.path().join(format!("{phase}.json")))["response"],
            expected
        );
        assert_eq!(expected["messages"][0], original);
        assert_eq!(paths(&root.path().join("home/ledger/messages")).len(), 1);
    }
}

#[test]
fn manager_mutation_returns_success_when_only_post_commit_prune_fails() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let outcome = read(root.path().join("prune_remove_fault.json"));
    assert!(
        outcome.get("response").is_some(),
        "committed manager mutation must not surface as caller failure: {outcome}"
    );
    assert!(outcome.get("error").is_none(), "{outcome}");
    let settled = receipt(root.path());
    assert_eq!(settled["status"], "pending");
    assert_eq!(
        paths(&root.path().join("home/ledger/manager-intents")).len(),
        1
    );
}

#[test]
fn manager_post_commit_prune_closes_admission_but_replays_the_original_tick() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let expected = read(root.path().join("prune_remove_fault.json"))["response"].clone();
    assert!(root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .is_file());
    assert_eq!(receipt(root.path())["status"], "pending");
    for phase in ["replay", "reopen2"] {
        exited(&child(root.path(), "tick-message", phase, ""), 0);
        assert_eq!(
            read(root.path().join(format!("{phase}.json")))["response"],
            expected
        );
    }
}

#[test]
fn manager_post_commit_prune_recovers_on_ordinary_reopen_without_test_phase_env() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let expected = read(root.path().join("prune_remove_fault.json"))["response"].clone();
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    assert_eq!(receipt(root.path())["status"], "complete");
    assert!(!root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .exists());
    assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
    assert_eq!(
        read(root.path().join("prune_remove_fault.json"))["response"],
        expected
    );
}

#[test]
fn manager_post_commit_prune_recovers_when_intent_unlinked_before_marker() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let marker_path = root
        .path()
        .join("home/ledger/ledger-admission-recovery.json");
    let intent_paths = paths(&root.path().join("home/ledger/manager-intents"));
    assert_eq!(intent_paths.len(), 1);
    let intent_path = intent_paths[0].clone();
    std::fs::remove_file(&intent_path).unwrap();
    assert!(marker_path.is_file());
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    assert!(!marker_path.exists());
    assert_eq!(receipt(root.path())["status"], "complete");
}

#[test]
fn manager_post_commit_prune_keeps_intent_when_prune_fails_again_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    PRUNE_REMOVE_FAULT.store(true, Ordering::SeqCst);
    {
        let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
        PRUNE_REMOVE_FAULT.store(false, Ordering::SeqCst);
        assert!(store.ledger_admission_closed().is_some());
        assert_eq!(
            receipt(root.path())["status"],
            "pending",
            "prune must not settle the caller receipt while recovery marker remains"
        );
        assert!(root
            .path()
            .join("home/ledger/ledger-admission-recovery.json")
            .is_file());
        assert_eq!(
            paths(&root.path().join("home/ledger/manager-intents")).len(),
            1
        );
    }
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    assert!(!root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .exists());
    assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
    assert_eq!(receipt(root.path())["status"], "complete");
}

#[test]
fn manager_post_commit_prune_restores_deleted_notification_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let outcome = read(root.path().join("prune_remove_fault.json"));
    let expected_message = outcome["response"]["messages"][0].clone();
    let message_id = expected_message["messageId"].as_str().unwrap();
    let message_path = root
        .path()
        .join("home/ledger/messages")
        .join(format!("{}.json", safe_id_filename(message_id).unwrap()));
    assert!(message_path.is_file());
    std::fs::remove_file(&message_path).unwrap();
    assert!(!message_path.exists());
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    assert!(!root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .exists());
    assert_eq!(receipt(root.path())["status"], "complete");
    assert!(message_path.is_file());
    assert_eq!(read(message_path), expected_message);
}

#[test]
fn manager_post_commit_prune_restores_deleted_child_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    // Retention pruning runs only when message count exceeds MAX_RETAINED_MESSAGES
    // (500). Advance does not append a notification, so seed one extra message so
    // post-commit prune fails while the mutation intent still seals created Work.
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":501}),
    );
    exited(&child(root.path(), "advance", "prune_remove_fault", ""), 0);
    let outcome = read(root.path().join("prune_remove_fault.json"));
    let created = outcome["response"]["createdWork"]
        .as_array()
        .expect("advance must seal created Work in its committed response");
    assert!(!created.is_empty(), "advance must create child Work");
    assert!(
        root.path()
            .join("home/ledger/ledger-admission-recovery.json")
            .is_file(),
        "post-commit prune failure must leave admission recovery evidence"
    );
    let expected_child = created[0].clone();
    let work_id = expected_child["workId"].as_str().unwrap();
    let work_path = root
        .path()
        .join("home/ledger/work-items")
        .join(format!("{}.json", safe_id_filename(work_id).unwrap()));
    assert!(work_path.is_file());
    std::fs::remove_file(&work_path).unwrap();
    assert!(!work_path.exists());
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(
        work_path.is_file(),
        "deleted child Work must be replayed before settlement"
    );
    assert_eq!(read(work_path), expected_child);
    assert!(store.ledger_admission_closed().is_none());
    assert_eq!(receipt(root.path())["status"], "complete");
}

#[test]
fn manager_post_commit_prune_marker_only_restores_deleted_notification_on_reopen() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let outcome = read(root.path().join("prune_remove_fault.json"));
    let expected_message = outcome["response"]["messages"][0].clone();
    let message_id = expected_message["messageId"].as_str().unwrap();
    let message_path = root
        .path()
        .join("home/ledger/messages")
        .join(format!("{}.json", safe_id_filename(message_id).unwrap()));
    let intent_paths = paths(&root.path().join("home/ledger/manager-intents"));
    assert_eq!(intent_paths.len(), 1);
    std::fs::remove_file(&intent_paths[0]).unwrap();
    std::fs::remove_file(&message_path).unwrap();
    assert!(root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .is_file());
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_none());
    assert_eq!(receipt(root.path())["status"], "complete");
    assert!(message_path.is_file());
    assert_eq!(read(message_path), expected_message);
}

#[test]
fn manager_post_commit_prune_refuses_altered_committed_effects() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let outcome = read(root.path().join("prune_remove_fault.json"));
    let message_id = outcome["response"]["messages"][0]["messageId"]
        .as_str()
        .unwrap();
    let message_path = root
        .path()
        .join("home/ledger/messages")
        .join(format!("{}.json", safe_id_filename(message_id).unwrap()));
    let mut message = read(message_path.clone());
    message["body"] = json!("tampered committed message effect");
    write(message_path, &message);
    let _guard = common::ProcessEnvGuard::new();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::env::remove_var("MANAGER_MUTATION_PHASE");
    let store = OrchStore::open(root.path().join("home/ledger")).unwrap();
    assert!(store.ledger_admission_closed().is_some());
    assert!(root
        .path()
        .join("home/ledger/ledger-admission-recovery.json")
        .is_file());
    assert_eq!(receipt(root.path())["status"], "pending");
}

#[test]
fn manager_post_commit_prune_refuses_tampered_marker_without_intent() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"seedMessages":500}),
    );
    exited(
        &child(root.path(), "tick-message", "prune_remove_fault", ""),
        0,
    );
    let marker_path = root
        .path()
        .join("home/ledger/ledger-admission-recovery.json");
    let intent_paths = paths(&root.path().join("home/ledger/manager-intents"));
    assert_eq!(intent_paths.len(), 1);
    std::fs::remove_file(&intent_paths[0]).unwrap();
    let marker = read(marker_path.clone());
    let cases: Vec<(&str, Value)> = vec![
        ("altered-response", {
            let mut tampered = marker.clone();
            tampered["sealedResponse"]["plan"]["revision"] = json!(
                tampered["sealedResponse"]["plan"]["revision"]
                    .as_u64()
                    .unwrap()
                    + 99
            );
            tampered
        }),
        ("altered-identity", {
            let mut tampered = marker.clone();
            tampered["sealedBinding"]["ownerId"] = json!("foreign-owner");
            tampered
        }),
        ("absent-sealed-binding", {
            let mut tampered = marker.clone();
            tampered.as_object_mut().unwrap().remove("sealedBinding");
            tampered
        }),
        ("absent-sealed-response", {
            let mut tampered = marker.clone();
            tampered.as_object_mut().unwrap().remove("sealedResponse");
            tampered
        }),
    ];
    for (label, tampered) in cases {
        let case_root = tempfile::tempdir().unwrap();
        copy_tree::copy_dir_all(root.path(), case_root.path()).unwrap();
        let case_marker = case_root
            .path()
            .join("home/ledger/ledger-admission-recovery.json");
        write(&case_marker, &tampered);
        let intent_paths = paths(&case_root.path().join("home/ledger/manager-intents"));
        for path in intent_paths {
            std::fs::remove_file(path).unwrap();
        }
        let _guard = common::ProcessEnvGuard::new();
        set_grokptah_home_override(Some(case_root.path().join("home")));
        std::env::remove_var("MANAGER_MUTATION_PHASE");
        let store = OrchStore::open(case_root.path().join("home/ledger")).unwrap();
        assert!(
            store.ledger_admission_closed().is_some(),
            "{label}: tampered marker must keep admission closed"
        );
        assert!(
            case_marker.is_file(),
            "{label}: tampered marker evidence must be preserved"
        );
        assert_eq!(
            receipt(case_root.path())["status"],
            "pending",
            "{label}: caller receipt must stay pending until validated recovery"
        );
    }
}

fn pre_intent_manager_process_death_has_no_mutation_effect() {
    for operation in ["advance", "tick", "replan", "tick-message"] {
        let root = tempfile::tempdir().unwrap();
        exited(
            &child(
                root.path(),
                operation,
                "cut",
                "manager_mutation_before_intent",
            ),
            86,
        );
        assert_eq!(receipt(root.path())["status"], "pending");
        assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
        let original = disk_json(root.path());
        let mut settled = None;
        for _ in 0..3 {
            exited(&child(root.path(), operation, "refusal", ""), 0);
            let claim = receipt(root.path());
            assert_eq!(claim["status"], "failed");
            let mut current = disk_json(root.path());
            let mut before = original.clone();
            current.retain(|path, _| {
                !path
                    .components()
                    .any(|part| part.as_os_str() == "idempotency")
            });
            before.retain(|path, _| {
                !path
                    .components()
                    .any(|part| part.as_os_str() == "idempotency")
            });
            assert_eq!(current, before);
            if let Some(first) = &settled {
                assert_eq!(&claim, first);
            } else {
                settled = Some(claim);
            }
        }
    }
}
