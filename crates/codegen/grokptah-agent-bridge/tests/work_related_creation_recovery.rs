//! Actual MCP callers and the scheduled writer, process death and fresh replay.
mod common;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    canonical_workspace, safe_id_filename, OrchStore, OrchestrationConfig, OrchestrationService,
    RunBounds, WorkAttempt, WorkDecision, WorkDecisionAction, WorkItem, WorkPolicy,
    WorkspaceAllowlist, MAX_GRAPH_SCOPE_ITEMS, WORKLOAD_SCHEMA_VERSION,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, DurableWriteFault, HostConfig,
    McpControlClient, SessionKind, DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use uuid::Uuid;

const TOKEN: &str = "synthetic-related-creation-v2";
const REQUEST: &str = "original-related-creation";
fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}
fn write(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn paths(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|e| e == "json"))
        .map(|e| e.into_path())
        .collect();
    paths.sort();
    paths
}
fn receipt_path(root: &Path) -> PathBuf {
    paths(&root.join("home/ledger/idempotency"))
        .into_iter()
        .find(|path| read(path)["requestId"] == REQUEST)
        .unwrap()
}
fn receipt(root: &Path) -> Value {
    read(receipt_path(root))
}
fn intent_dir(root: &Path, family: &str) -> PathBuf {
    root.join("home/ledger").join(if family == "manager" {
        "manager-intents"
    } else {
        "routine-intents"
    })
}
fn child(root: &Path, family: &str, phase: &str, stage: &str) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "related_creation_child", "--nocapture"])
        .env("RELATED_ROOT", root)
        .env("RELATED_FAMILY", family)
        .env("RELATED_PHASE", phase)
        .env("RELATED_STAGE", stage)
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
fn expected_response(root: &Path, family: &str) -> Value {
    let intents = paths(&intent_dir(root, family));
    if intents.is_empty() {
        return receipt(root)["response"].clone();
    }
    assert_eq!(intents.len(), 1);
    let intent = read(&intents[0]);
    if family == "manager" {
        json!({"plan":intent["plan"],"rootWork":intent["rootWork"]})
    } else {
        json!({"activation":intent["activation"],"routineId":intent["routine"]["routineId"],"sessionId":intent["routine"]["sessionId"],"workspace":intent["request"]["binding"].get("workspaceDisplay").unwrap_or(&intent["routine"]["workspace"])})
    }
}

#[test]
fn related_creation_child() {
    let Ok(root) = std::env::var("RELATED_ROOT") else {
        return;
    };
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = PathBuf::from(root);
    set_grokptah_home_override(Some(root.join("home")));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_child(root, &mut env));
}

async fn run_child(root: PathBuf, env: &mut common::ProcessEnvGuard) {
    let family = std::env::var("RELATED_FAMILY").unwrap();
    let phase = std::env::var("RELATED_PHASE").unwrap();
    let stage = std::env::var("RELATED_STAGE").unwrap();
    let options = if root.join("options.json").exists() {
        read(root.join("options.json"))
    } else {
        json!({})
    };
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
    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
    client.initialize().await.unwrap();
    let args = if setup {
        let agent = host.ensure_session_agent(session).unwrap();
        let args = if family == "manager" {
            json!({"request_id":REQUEST,"session_id":session,"workspace":workspace,"manager_agent_id":agent.agent_id,
                "objective":"original manager root","steps":[{"stepId":"inspect","kind":"verification","objective":"inspect synthetic fixture"}],"max_in_flight":1,"max_replans":2})
        } else {
            let trigger = if family == "scheduled" {
                json!({"kind":"one_shot","at":Utc::now()-chrono::Duration::seconds(1)})
            } else {
                json!({"kind":"manual"})
            };
            let routine=client.call_tool("ptah_create_routine",json!({"request_id":"setup-routine","session_id":session,"workspace":workspace,
                "name":"recoverable activation","agent_id":agent.agent_id,"trigger":trigger,"work_template":{"kind":"routine","objective":"original routine work",
                    "policy":{"bounds":{"maxPromptBytes":100000,"maxRounds":4,"maxDurationMs":60000},"retry":{"maxAttempts":3,"retryFailed":true,"retryExpired":true,"backoffMs":0},"requiresApproval":false,"maxConcurrentAttempts":1}}})).await.unwrap();
            let routine_id = &routine.structured["routine"]["routineId"];
            if options["disabled"] == true {
                client.call_tool("ptah_disable_routine",json!({"request_id":"setup-disable","session_id":session,"workspace":workspace,"routine_id":routine_id})).await.unwrap();
            }
            if options["missingAgent"] == true {
                let path = paths(&store.root().join("agents"))
                    .into_iter()
                    .find(|path| read(path)["agentId"] == agent.agent_id)
                    .unwrap();
                std::fs::remove_file(path).unwrap();
            }
            let payload = if options["secret"] == true {
                json!({"source":"synthetic-original","password":"synthetic-do-not-persist","nested":{"api_key":"synthetic-nested-secret"}})
            } else {
                json!({"source":"synthetic-original"})
            };
            json!({"request_id":REQUEST,"session_id":session,"workspace":workspace,"routine_id":routine_id,"payload":payload})
        };
        let mut args = args;
        if options["workspaceAlias"] == true {
            args["workspace"] = json!(format!("{}/.", workspace.display()));
        }
        write(
            root.join("fixture.json"),
            &json!({"sessionId":session,"args":args}),
        );
        for index in 0..options["seedCount"].as_u64().unwrap_or(0) {
            let mut item = WorkItem::new(
                "test",
                "synthetic capacity fixture",
                session,
                workspace.display().to_string(),
                "synthetic-fixture",
                WorkPolicy::default(),
            )
            .unwrap();
            item.work_id = format!("seed-{index:04}");
            // Synthetic fixtures only. The final admission uses the actual caller.
            write(
                store
                    .root()
                    .join("work-items")
                    .join(format!("{}.json", safe_id_filename(&item.work_id).unwrap())),
                &json!(item),
            );
        }
        args
    } else {
        read(root.join("fixture.json"))["args"].clone()
    };
    if phase == "create" {
        env.set("GROKPTAH_CRASH_STAGE", &stage);
    }
    if phase == "fault" {
        let (needle, skips_remaining) = match stage.as_str() {
            "before_intent" => (
                if family == "manager" {
                    "manager-intents"
                } else {
                    "routine-intents"
                },
                0,
            ),
            "work" => ("work-items", 0),
            "plan" => ("manager-plans", 0),
            "routine" => ("routines", 0),
            "activation" => ("routine-activations", 0),
            "dedupe" => ("routine-dedupe", 0),
            "receipt" => ("idempotency/v2", 0),
            "cleanup" => (
                if family == "manager" {
                    "manager-intents"
                } else {
                    "routine-intents"
                },
                1,
            ),
            other => panic!("unknown fault {other}"),
        };
        *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
            needle,
            skips_remaining,
        });
    }
    let outcome = if family == "scheduled" {
        if setup {
            json!(store.fire_due_routines_at(Utc::now()).unwrap())
        } else {
            json!({"recovered":true})
        }
    } else {
        let tool = if family == "manager" {
            "ptah_create_manager_plan"
        } else {
            "ptah_fire_routine"
        };
        match client.call_tool(tool, args.clone()).await {
            Ok(result) => {
                assert!(!result.is_error, "{}", result.structured);
                result.structured
            }
            Err(error) if phase == "fault" || phase == "refusal" => {
                json!({"error":error.to_string()})
            }
            Err(error) => panic!(
                "MCP replay failed: {error}; durable receipt: {}",
                receipt(&root)
            ),
        }
    };
    *DURABLE_WRITE_FAULT.lock().unwrap() = None;
    write(
        root.join(format!("{phase}.json")),
        &json!({"response":outcome,"admissionClosed":store.ledger_admission_closed()}),
    );
    if phase == "fault" && stage != "before_intent" {
        assert!(outcome.get("error").is_some());
        assert!(store.ledger_admission_closed().is_some());
        let mut later = args;
        later["request_id"] = json!("must-not-admit-after-uncertainty");
        assert!(client
            .call_tool(
                if family == "manager" {
                    "ptah_create_manager_plan"
                } else {
                    "ptah_fire_routine"
                },
                later
            )
            .await
            .is_err());
        assert!(store.fire_due_routines_at(Utc::now()).is_err());
    }
    if outcome.get("error").is_none() {
        let expected: BTreeSet<_> = store
            .list_work_items()
            .unwrap()
            .into_iter()
            .map(|item| item.work_id)
            .collect();
        // Walk the full recovered capacity lane once. The two additional
        // fresh processes still check all stored IDs and the exact receipt,
        // without repeating the same expensive whole-lane traversal.
        assert_eq!(
            expected.len(),
            paths(&store.root().join("work-items")).len()
        );
        let repeat_capacity_read =
            expected.len() > 200 && matches!(phase.as_str(), "reopen2" | "reopen3");
        let mut orders = Vec::new();
        for (tool, field) in [
            ("ptah_list_work_page", "items"),
            ("ptah_get_work_graph_page", "nodes"),
        ] {
            if repeat_capacity_read {
                continue;
            }
            let mut cursor = None;
            let mut ids = Vec::new();
            let mut complete = false;
            for _ in 0..100 {
                let mut args = json!({"session_id":session,"workspace":workspace,"limit":200});
                if let Some(cursor) = &cursor {
                    args["cursor"] = json!(cursor);
                }
                let page = client.call_tool(tool, args).await.unwrap();
                assert!(!page.is_error);
                ids.extend(
                    page.structured[field]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|row| row["workId"].as_str().unwrap().to_string()),
                );
                if page.structured["complete"] == true {
                    complete = true;
                    break;
                }
                let next = page.structured["continuation"]
                    .as_str()
                    .unwrap()
                    .to_string();
                assert_ne!(cursor.as_ref(), Some(&next));
                cursor = Some(next);
            }
            assert!(complete);
            assert_eq!(ids.len(), expected.len());
            assert_eq!(ids.iter().cloned().collect::<BTreeSet<_>>(), expected);
            orders.push(ids);
        }
        if !repeat_capacity_read {
            assert_eq!(orders[0], orders[1]);
        }
    }
    assert!(server.stop_and_wait().await.fully_stopped);
    assert!(orch.stop_background_tasks().await.fully_stopped);
    assert!(host.shutdown().await.is_clean());
}

fn assert_replay(root: &Path, family: &str, expected: &Value) {
    let original = receipt(root);
    for reopen in ["replay", "reopen2", "reopen3"] {
        exited(&child(root, family, reopen, ""), 0);
        let outcome = read(root.join(format!("{reopen}.json")));
        assert_eq!(outcome["response"], *expected);
        assert!(outcome["admissionClosed"].is_null());
        let settled = receipt(root);
        assert_eq!(settled["status"], "complete");
        assert_eq!(settled["response"], *expected);
        for field in [
            "payloadHash",
            "ownerId",
            "sessionId",
            "workspaceDigest",
            "tool",
            "requestId",
        ] {
            assert_eq!(settled[field], original[field]);
        }
        assert_eq!(
            settled["runId"],
            if family == "manager" {
                expected["plan"]["planId"].clone()
            } else {
                expected["routineId"].clone()
            }
        );
        assert!(paths(&intent_dir(root, family)).is_empty());
        assert!(paths(&root.join("home/ledger/work-attempts")).is_empty());
        assert!(paths(&root.join("home/ledger/work-decisions")).is_empty());
    }
}
fn prove(family: &str, stage: &str) {
    let root = tempfile::tempdir().unwrap();
    exited(&child(root.path(), family, "create", stage), 86);
    let expected = expected_response(root.path(), family);
    assert!(!expected.is_null());
    assert_eq!(
        receipt(root.path())["status"],
        if stage.contains("after_receipt")
            || stage.contains("after_intent_remove")
            || stage.contains("after_commit")
        {
            "complete"
        } else {
            "pending"
        }
    );
    assert_replay(root.path(), family, &expected);
    assert_eq!(paths(&root.path().join("home/ledger/work-items")).len(), 1);
    assert_eq!(
        paths(&root.path().join(if family == "manager" {
            "home/ledger/manager-plans"
        } else {
            "home/ledger/routine-activations"
        }))
        .len(),
        1
    );
}
#[test]
fn manager_creation_replays_after_work_write() {
    prove("manager", "manager_create_after_work");
}
#[test]
fn routine_creation_replays_after_work_write() {
    prove("routine", "routine_fire_after_work");
}
#[test]
fn related_creation_cut_matrix_replays_exact_outcomes_in_three_processes() {
    for stage in [
        "after_intent",
        "after_plan",
        "after_receipt",
        "after_intent_remove",
        "after_commit",
    ] {
        prove("manager", &format!("manager_create_{stage}"));
    }
    for stage in [
        "after_intent",
        "after_routine",
        "after_activation",
        "after_dedupe",
        "after_receipt",
        "after_intent_remove",
        "after_commit",
    ] {
        prove("routine", &format!("routine_fire_{stage}"));
    }
}
#[test]
fn pre_intent_crashes_have_no_created_work_or_guessed_success() {
    for family in ["manager", "routine"] {
        let root = tempfile::tempdir().unwrap();
        let stage = if family == "manager" {
            "manager_create_before_intent"
        } else {
            "routine_fire_before_intent"
        };
        exited(&child(root.path(), family, "create", stage), 86);
        assert_eq!(receipt(root.path())["status"], "pending");
        for _ in 0..3 {
            exited(&child(root.path(), family, "refusal", ""), 0);
            assert_eq!(receipt(root.path())["status"], "failed");
        }
        assert!(paths(&root.path().join("home/ledger/work-items")).is_empty());
        assert!(paths(&intent_dir(root.path(), family)).is_empty());
    }
}
#[test]
fn admitted_write_and_cleanup_faults_keep_receipts_and_close_admission() {
    for family in ["manager", "routine"] {
        let faults = if family == "manager" {
            vec!["work", "plan", "receipt", "cleanup"]
        } else {
            vec!["work", "activation", "dedupe", "receipt", "cleanup"]
        };
        for stage in faults {
            let root = tempfile::tempdir().unwrap();
            exited(&child(root.path(), family, "fault", stage), 0);
            assert_eq!(
                receipt(root.path())["status"],
                if stage == "cleanup" {
                    "complete"
                } else {
                    "pending"
                }
            );
            let expected = expected_response(root.path(), family);
            assert_replay(root.path(), family, &expected);
            assert_eq!(paths(&root.path().join("home/ledger/work-items")).len(), 1);
        }
        let root = tempfile::tempdir().unwrap();
        exited(&child(root.path(), family, "fault", "before_intent"), 0);
        assert_eq!(receipt(root.path())["status"], "failed");
        assert!(read(root.path().join("fault.json"))["admissionClosed"].is_null());
        assert!(paths(&root.path().join("home/ledger/work-items")).is_empty());
        assert!(paths(&intent_dir(root.path(), family)).is_empty());
    }
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    paths(&root.join("home/ledger"))
        .into_iter()
        .map(|path| {
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect()
}
#[test]
fn related_creation_corruptions_refuse_without_overwriting_evidence() {
    for family in ["manager", "routine"] {
        for variant in [
            "snapshot",
            "schema",
            "owner",
            "session",
            "workspace",
            "payloadHash",
            "requestPayload",
            "failedReceipt",
            "missingReceipt",
            "malformedReceipt",
            "receiptResponse",
            "filename",
            "foreignWork",
            "malformedIntent",
            "foreignOther",
            "extraAttempt",
            "malformedAttempt",
            "extraDecision",
            "malformedDecision",
        ] {
            let root = tempfile::tempdir().unwrap();
            let stage = if family == "manager" {
                "manager_create_after_work"
            } else {
                "routine_fire_after_dedupe"
            };
            exited(&child(root.path(), family, "create", stage), 86);
            let intent_path = paths(&intent_dir(root.path(), family))[0].clone();
            let mut intent = read(&intent_path);
            let receipt_path = receipt_path(root.path());
            let mut receipt = read(&receipt_path);
            match variant {
                "snapshot" => {
                    intent[if family == "manager" {
                        "rootWork"
                    } else {
                        "work"
                    }]["objective"] = json!("changed sealed objective");
                    write(&intent_path, &intent);
                }
                "schema" => {
                    intent["schemaVersion"] = json!(99);
                    write(&intent_path, &intent);
                }
                "owner" | "session" | "workspace" | "payloadHash" => {
                    let field = match variant {
                        "owner" => "ownerId",
                        "session" => "sessionId",
                        "workspace" => "workspaceDigest",
                        _ => "payloadHash",
                    };
                    intent["request"]["binding"][field] = json!(if variant == "session" {
                        Uuid::new_v4().to_string()
                    } else {
                        "foreign-evidence".into()
                    });
                    write(&intent_path, &intent);
                }
                "requestPayload" => {
                    intent["request"]["payload"]["workspace"] = json!("/foreign-input");
                    write(&intent_path, &intent);
                }
                "failedReceipt" => {
                    receipt["status"] = json!("failed");
                    write(&receipt_path, &receipt);
                }
                "missingReceipt" => std::fs::remove_file(receipt_path).unwrap(),
                "malformedReceipt" => std::fs::write(receipt_path, b"{broken").unwrap(),
                "receiptResponse" => {
                    receipt["status"] = json!("complete");
                    receipt["response"] = json!({"foreign":"outcome"});
                    write(&receipt_path, &receipt);
                }
                "filename" => std::fs::rename(
                    &intent_path,
                    intent_path.with_file_name("foreign-name.json"),
                )
                .unwrap(),
                "foreignWork" => {
                    let path = paths(&root.path().join("home/ledger/work-items"))[0].clone();
                    let mut work = read(&path);
                    work["objective"] = json!("foreign valid Work");
                    write(path, &work);
                }
                "malformedIntent" => std::fs::write(intent_path, b"{broken").unwrap(),
                "extraAttempt" | "malformedAttempt" => {
                    let work = &intent[if family == "manager" {
                        "rootWork"
                    } else {
                        "work"
                    }];
                    let attempt = WorkAttempt::new(
                        work["workId"].as_str().unwrap(),
                        1,
                        "synthetic-foreign-actor",
                        "synthetic-foreign-lease",
                    );
                    attempt.validate().unwrap();
                    let path = root.path().join("home/ledger/work-attempts").join(format!(
                        "{}.json",
                        safe_id_filename(&attempt.attempt_id).unwrap()
                    ));
                    if variant == "malformedAttempt" {
                        std::fs::write(path, b"{broken").unwrap();
                    } else {
                        write(path, &json!(attempt));
                    }
                }
                "extraDecision" | "malformedDecision" => {
                    let work = &intent[if family == "manager" {
                        "rootWork"
                    } else {
                        "work"
                    }];
                    let decision = WorkDecision {
                        schema_version: WORKLOAD_SCHEMA_VERSION,
                        decision_id: Uuid::new_v4().to_string(),
                        work_id: work["workId"].as_str().unwrap().into(),
                        action: WorkDecisionAction::Reprioritize,
                        actor_id: "synthetic-foreign-actor".into(),
                        actor_agent_id: None,
                        assigned_agent_id: None,
                        policy_revision: None,
                        work_revision: Some(3),
                        reason: "foreign decision evidence".into(),
                        created_at: Utc::now(),
                    };
                    decision.validate().unwrap();
                    let folder = root
                        .path()
                        .join("home/ledger/work-decisions")
                        .join(safe_id_filename(&decision.work_id).unwrap());
                    std::fs::create_dir_all(&folder).unwrap();
                    let path = folder.join(format!(
                        "{}.json",
                        safe_id_filename(&decision.decision_id).unwrap()
                    ));
                    if variant == "malformedDecision" {
                        std::fs::write(path, b"{broken").unwrap();
                    } else {
                        write(path, &json!(decision));
                    }
                }
                "foreignOther" => {
                    let path = paths(&root.path().join(if family == "manager" {
                        "home/ledger/manager-plans"
                    } else {
                        "home/ledger/routine-dedupe"
                    }));
                    if family == "manager" {
                        let mut plan = intent["plan"].clone();
                        plan["objective"] = json!("foreign plan");
                        let filename = safe_id_filename(plan["planId"].as_str().unwrap()).unwrap();
                        write(
                            root.path()
                                .join("home/ledger/manager-plans")
                                .join(format!("{filename}.json")),
                            &plan,
                        );
                    } else {
                        let mut dedupe = read(&path[0]);
                        dedupe["activationId"] = json!(Uuid::new_v4());
                        write(&path[0], &dedupe);
                    }
                }
                _ => unreachable!(),
            }
            let before = snapshot(root.path());
            for _ in 0..3 {
                exited(&child(root.path(), family, "replay", ""), 101);
                assert_eq!(snapshot(root.path()), before, "{family} {variant}");
            }
        }
    }
}
#[test]
fn related_creation_refuses_full_capacity_before_intent_admission() {
    for family in ["manager", "routine"] {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path().join("options.json"),
            &json!({"seedCount":MAX_GRAPH_SCOPE_ITEMS}),
        );
        exited(&child(root.path(), family, "refusal", ""), 0);
        assert_eq!(receipt(root.path())["status"], "failed");
        assert_eq!(receipt(root.path())["error"]["code"], "capacity_exhausted");
        assert!(paths(&intent_dir(root.path(), family)).is_empty());
        assert_eq!(
            paths(&root.path().join("home/ledger/work-items")).len(),
            MAX_GRAPH_SCOPE_ITEMS
        );
        assert!(paths(&root.path().join("home/ledger/manager-plans")).is_empty());
        assert!(paths(&root.path().join("home/ledger/routine-activations")).is_empty());
        exited(&child(root.path(), family, "refusal", ""), 0);
    }
}
#[test]
fn routine_non_work_and_backoff_outcomes_recover_without_an_extra_failure_increment() {
    for option in ["disabled", "missingAgent"] {
        for stage in [
            "routine_fire_after_intent",
            "routine_fire_after_routine",
            "routine_fire_after_activation",
            "routine_fire_after_receipt",
        ] {
            let root = tempfile::tempdir().unwrap();
            write(root.path().join("options.json"), &json!({option:true}));
            exited(&child(root.path(), "routine", "create", stage), 86);
            let expected = expected_response(root.path(), "routine");
            assert!(expected["activation"]["workId"].is_null());
            assert_replay(root.path(), "routine", &expected);
            assert!(paths(&root.path().join("home/ledger/work-items")).is_empty());
            let routine = read(&paths(&root.path().join("home/ledger/routines"))[0]);
            assert_eq!(
                routine["consecutiveFailures"],
                if option == "missingAgent" { 1 } else { 0 }
            );
        }
    }
}
#[test]
fn routine_intent_and_replayed_activation_keep_existing_secret_redaction() {
    let root = tempfile::tempdir().unwrap();
    write(root.path().join("options.json"), &json!({"secret":true}));
    exited(
        &child(root.path(), "routine", "create", "routine_fire_after_work"),
        86,
    );
    for path in paths(&root.path().join("home/ledger")) {
        let text = std::fs::read_to_string(path).unwrap();
        assert!(!text.contains("synthetic-do-not-persist"));
        assert!(!text.contains("synthetic-nested-secret"));
    }
    let expected = expected_response(root.path(), "routine");
    assert_replay(root.path(), "routine", &expected);
    assert_ne!(
        expected["activation"]["payload"]["password"],
        "synthetic-do-not-persist"
    );
}
#[test]
fn legacy_unbound_related_envelopes_recover_without_inventing_an_authenticated_outcome() {
    for family in ["manager", "routine"] {
        let root = tempfile::tempdir().unwrap();
        let stage = if family == "manager" {
            "manager_create_after_work"
        } else {
            "routine_fire_after_work"
        };
        exited(&child(root.path(), family, "create", stage), 86);
        let path = paths(&intent_dir(root.path(), family))[0].clone();
        let mut intent = read(&path);
        for field in [
            "schemaVersion",
            "request",
            "snapshotDigest",
            "priorPlan",
            "priorRootWork",
            "priorRoutine",
            "priorActivation",
            "priorWork",
        ] {
            intent.as_object_mut().unwrap().remove(field);
        }
        write(path, &intent);
        for _ in 0..3 {
            exited(&child(root.path(), family, "refusal", ""), 0);
            assert_eq!(receipt(root.path())["status"], "failed");
        }
        assert!(paths(&intent_dir(root.path(), family)).is_empty());
        assert_eq!(paths(&root.path().join("home/ledger/work-items")).len(), 1);
    }
}
#[test]
fn scheduled_writer_recovers_under_its_dedupe_authority_without_a_foreground_receipt() {
    let root = tempfile::tempdir().unwrap();
    exited(
        &child(
            root.path(),
            "scheduled",
            "create",
            "routine_fire_after_work",
        ),
        86,
    );
    let intent = read(&paths(&intent_dir(root.path(), "scheduled"))[0]);
    assert!(intent["request"].is_null());
    let work = intent["work"].clone();
    let activation = intent["activation"].clone();
    for phase in ["replay", "reopen2", "reopen3"] {
        exited(&child(root.path(), "scheduled", phase, ""), 0);
        assert_eq!(
            read(&paths(&root.path().join("home/ledger/work-items"))[0]),
            work
        );
        assert_eq!(
            read(&paths(&root.path().join("home/ledger/routine-activations"))[0]),
            activation
        );
        assert!(!paths(&root.path().join("home/ledger/idempotency"))
            .iter()
            .any(|path| read(path)["requestId"] == REQUEST));
    }
}

#[test]
fn pending_related_receipt_conflicts_are_preserved_before_any_recovery_write() {
    for family in ["manager", "routine"] {
        for field in ["response", "error", "runId"] {
            let root = tempfile::tempdir().unwrap();
            let stage = if family == "manager" {
                "manager_create_after_work"
            } else {
                "routine_fire_after_work"
            };
            exited(&child(root.path(), family, "create", stage), 86);
            let path = receipt_path(root.path());
            let mut value = read(&path);
            value[field] = match field {
                "error" => json!({"code":"internal","message":"foreign pending error"}),
                "response" => json!({"foreign":"pending response"}),
                _ => json!("foreign pending subject"),
            };
            write(path, &value);
            let before = snapshot(root.path());
            for _ in 0..3 {
                exited(&child(root.path(), family, "replay", ""), 101);
                assert_eq!(snapshot(root.path()), before, "{family} {field}");
            }
        }
    }
}

#[test]
fn recovered_related_creation_occupies_the_final_slot_once_and_remains_pageable() {
    for family in ["manager", "routine"] {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path().join("options.json"),
            &json!({"seedCount":MAX_GRAPH_SCOPE_ITEMS - 1}),
        );
        exited(
            &child(
                root.path(),
                family,
                "create",
                if family == "manager" {
                    "manager_create_after_work"
                } else {
                    "routine_fire_after_work"
                },
            ),
            86,
        );
        let expected = expected_response(root.path(), family);
        assert_replay(root.path(), family, &expected);
        assert_eq!(
            paths(&root.path().join("home/ledger/work-items")).len(),
            MAX_GRAPH_SCOPE_ITEMS
        );
    }
}

#[test]
fn routine_backoff_write_fault_leaves_its_prior_and_original_receipt_recoverable() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"missingAgent":true}),
    );
    exited(&child(root.path(), "routine", "fault", "routine"), 0);
    let path = paths(&intent_dir(root.path(), "routine"))[0].clone();
    let intent = read(path);
    let routine = read(&paths(&root.path().join("home/ledger/routines"))[0]);
    assert_eq!(routine, intent["priorRoutine"]);
    assert_eq!(receipt(root.path())["status"], "pending");
    let expected = expected_response(root.path(), "routine");
    assert_replay(root.path(), "routine", &expected);
    assert_eq!(
        read(&paths(&root.path().join("home/ledger/routines"))[0])["consecutiveFailures"],
        1
    );
}

#[test]
fn manual_routine_from_a_canonical_alias_replays_the_original_response_workspace() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path().join("options.json"),
        &json!({"workspaceAlias":true}),
    );
    exited(
        &child(root.path(), "routine", "create", "routine_fire_after_work"),
        86,
    );
    let expected = expected_response(root.path(), "routine");
    let input = read(root.path().join("fixture.json"))["args"]["workspace"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        expected["workspace"],
        canonical_workspace(Path::new(&input))
            .unwrap()
            .display()
            .to_string()
    );
    assert_ne!(expected["workspace"], input);
    assert_replay(root.path(), "routine", &expected);
}
