//! Production authenticated Work creation, process death and fresh-process replay.
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, hash_payload, safe_id_filename, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, WorkItem, WorkPolicy, WorkspaceAllowlist,
    MAX_GRAPH_SCOPE_ITEMS, WORK_PAGE_TRANSPORT_CEILING_BYTES,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, DurableWriteFault, HostConfig,
    McpControlClient, SessionKind, DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use uuid::Uuid;

const BEARER: &str = "synthetic-work-creation-v2";
const REQUEST: &str = "create-original-request";

fn read_json(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write_json(path: impl AsRef<Path>, value: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn records(dir: &Path) -> Vec<Value> {
    let mut paths = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    paths.into_iter().map(read_json).collect()
}

fn json_paths(dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths.extend(json_paths(&path));
        } else if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    paths.sort();
    paths
}

fn receipt_path(root: &Path) -> PathBuf {
    let paths = json_paths(&root.join("ledger/idempotency"));
    paths
        .into_iter()
        .find(|path| read_json(path)["requestId"] == REQUEST)
        .unwrap()
}

fn intent_paths(root: &Path) -> Vec<PathBuf> {
    json_paths(&root.join("ledger/work-lifecycle-intents"))
}

fn child(root: &Path, phase: &str, stage: &str) -> Output {
    child_with(root, phase, stage, &[])
}

fn child_with(root: &Path, phase: &str, stage: &str, extra: &[(&str, &str)]) -> Output {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "creation_service_child", "--nocapture"])
        .env("WORK_CREATION_CHILD_ROOT", root)
        .env("WORK_CREATION_PHASE", phase)
        .env("WORK_CREATION_STAGE", stage)
        .env_remove("GROKPTAH_CRASH_STAGE")
        .env_remove("WORK_CREATION_WORKSPACE_ALIAS");
    if phase == "create" {
        command.env("GROKPTAH_CRASH_STAGE", stage);
    }
    command.envs(extra.iter().copied()).output().unwrap()
}

fn assert_exit(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn creation_service_child() {
    let Ok(root) = std::env::var("WORK_CREATION_CHILD_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("WORK_CREATION_PHASE").unwrap();
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    set_grokptah_home_override(Some(root.join("home")));
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            std::fs::create_dir_all(root.join("workspace")).unwrap();
            let workspace = canonical_workspace(&root.join("workspace")).unwrap();
            let host = AgentHost::create(HostConfig {
                always_approve: true,
                ..HostConfig::default()
            })
            .unwrap();
            host.start().unwrap();
            let session = if phase == "create" || phase == "fault" {
                host.session_new_kind(SessionKind::Build).unwrap().id
            } else {
                let meta = read_json(root.join("fixture.json"));
                let id = Uuid::parse_str(meta["sessionId"].as_str().unwrap()).unwrap();
                host.session_inspect(id).expect("reload original session");
                id
            };
            host.session_set_cwd(session, &workspace).unwrap();
            let store = OrchStore::open(root.join("ledger")).unwrap();
            if phase == "create" {
                let count: usize = std::env::var("WORK_CREATION_SEED_COUNT")
                    .unwrap_or_else(|_| "0".into()).parse().unwrap();
                for index in 0..count {
                    let mut item = WorkItem::new("test", "capacity fixture", session,
                        workspace.display().to_string(), "synthetic-fixture", WorkPolicy::default()).unwrap();
                    item.work_id = format!("seed-{index:04}");
                    item.validate().unwrap();
                    // Fixture setup only; the final admitted create below is
                    // the actual service/store capacity authority.
                    write_json(store.root().join("work-items").join(format!("{}.json", safe_id_filename(&item.work_id).unwrap())), &json!(item));
                }
            }
            let orch = OrchestrationService::new(
                host.clone(),
                host.event_bus(),
                store.clone(),
                OrchestrationConfig {
                    bearer_token: BEARER.into(),
                    allowlist: WorkspaceAllowlist::new([workspace.clone()]),
                    max_concurrent_runs: 2,
                    bounds: RunBounds::default(),
                },
            );
            let auth = orch.auth_header(Some(&format!("Bearer {BEARER}"))).unwrap();
            if phase == "create" || phase == "fault" {
                write_json(
                    root.join("fixture.json"),
                    &json!({"sessionId":session,"ownerId":auth.owner_id,"workspace":workspace}),
                );
                let stage = std::env::var("WORK_CREATION_STAGE").unwrap();
                if phase == "fault" {
                    let (needle, skips_remaining) = match stage.as_str() {
                        "before_intent" => ("work-lifecycle-intents", 0),
                        "work" => ("work-items", 0),
                        "receipt" => ("idempotency/v2", 0),
                        "cleanup" => ("work-lifecycle-intents", 1),
                        other => panic!("unknown fault {other}"),
                    };
                    *DURABLE_WRITE_FAULT.lock().unwrap() = Some(DurableWriteFault {
                        needle,
                        skips_remaining,
                    });
                } else {
                    env.set("GROKPTAH_CRASH_STAGE", stage);
                }
            }
            let invoke_workspace = std::env::var("WORK_CREATION_WORKSPACE_ALIAS")
                .map(PathBuf::from)
                .unwrap_or_else(|_| workspace.clone());
            if phase == "create" && std::env::var("WORK_CREATION_WORKSPACE_ALIAS").is_ok() {
                let stopped = orch.stop_background_tasks().await;
                assert!(
                    stopped.fully_stopped && stopped.errors.is_empty(),
                    "aliased intent-cut create must not race the workload supervisor"
                );
            }
            let result = orch
                .create_work(
                    &auth,
                    REQUEST,
                    session,
                    &invoke_workspace,
                    "test".into(),
                    "recover the original Work creation".into(),
                    7,
                    None,
                    None,
                    Vec::new(),
                    WorkPolicy::default(),
                )
                .await;
            *DURABLE_WRITE_FAULT.lock().unwrap() = None;
            let mut outcome = match result {
                Ok(response) => json!({"response":response}),
                Err(error) => json!({"error":error}),
            };
            outcome["admissionClosed"] = json!(store.ledger_admission_closed());
            if phase == "read" {
                assert!(outcome.get("response").is_some());
                let server = start_control_server(orch.clone(), 0).await.unwrap();
                let base = format!("http://{}", server.addr);
                let mut client = McpControlClient::new(&base, BEARER);
                client.initialize().await.unwrap();
                let header = client.session_id().unwrap().to_string();
                let expected = store.list_work_items().unwrap().into_iter()
                    .map(|item| item.work_id).collect::<std::collections::BTreeSet<_>>();
                let http = reqwest::Client::new();
                let mut largest = 0;
                let mut traversals = Vec::new();
                for (tool, field) in [("ptah_list_work_page", "items"), ("ptah_get_work_graph_page", "nodes")] {
                    let mut cursor = None;
                    let mut ids = Vec::new();
                    let mut completed = false;
                    for page_number in 0..100 {
                        let mut args = json!({"session_id":session,"workspace":workspace,"limit":200});
                        if let Some(token) = &cursor { args["cursor"] = json!(token); }
                        let response = http.post(format!("{base}/mcp"))
                            .bearer_auth(BEARER).header("Content-Type", "application/json")
                            .header("Accept", "application/json, text/event-stream")
                            .header("MCP-Protocol-Version", "2025-03-26")
                            .header("mcp-session-id", &header)
                            .json(&json!({"jsonrpc":"2.0","id":page_number,"method":"tools/call","params":{"name":tool,"arguments":args}}))
                            .send().await.unwrap();
                        assert!(response.status().is_success());
                        let bytes = response.bytes().await.unwrap();
                        largest = largest.max(bytes.len());
                        assert!(bytes.len() <= WORK_PAGE_TRANSPORT_CEILING_BYTES);
                        let envelope: Value = serde_json::from_slice(&bytes).unwrap();
                        assert!(envelope.get("error").is_none(), "{envelope}");
                        let page = &envelope["result"]["structuredContent"];
                        let entries = page[field].as_array().expect("paged entries");
                        ids.extend(entries.iter().map(|entry| entry["workId"].as_str().unwrap().to_string()));
                        if page["complete"] == true { completed=true; break; }
                        assert!(!entries.is_empty());
                        let next = page["continuation"].as_str().unwrap().to_string();
                        assert_ne!(cursor.as_ref(), Some(&next));
                        cursor = Some(next);
                    }
                    assert!(completed);
                    assert_eq!(ids.len(), expected.len());
                    assert_eq!(ids.iter().cloned().collect::<std::collections::BTreeSet<_>>(), expected);
                    traversals.push(ids);
                }
                assert_eq!(traversals[0], traversals[1]);
                let work_id = outcome["response"]["work"]["workId"].as_str().unwrap();
                let summary = client.call_tool("ptah_get_work_detail", json!({"session_id":session,"workspace":workspace,"work_id":work_id,"section":"summary","limit":1})).await.unwrap();
                for field in ["workId", "revision", "state", "sessionId", "workspace", "createdBy", "createdAt", "updatedAt"] {
                    assert_eq!(summary.structured["body"][field], outcome["response"]["work"][field], "recovered summary {field}");
                }
                let objective = client.call_tool("ptah_get_work_detail", json!({"session_id":session,"workspace":workspace,"work_id":work_id,"section":"objective","limit":1})).await.unwrap();
                assert_eq!(objective.structured["body"]["objective"], outcome["response"]["work"]["objective"]);
                outcome["wireMaxBytes"] = json!(largest);
                outcome["traversedIds"] = json!(expected.len());
                if expected.len() == MAX_GRAPH_SCOPE_ITEMS {
                    let overflow = orch.create_work(&auth, "one-beyond-capacity", session, &workspace,
                        "test".into(), "refuse without another Work".into(), 0, None, None, Vec::new(), WorkPolicy::default())
                        .await.expect_err("4097th Work must refuse");
                    assert_eq!(overflow.code, grokptah_agent_bridge::orchestration::OrchErrorCode::CapacityExhausted);
                    assert_eq!(store.list_work_items().unwrap().len(), MAX_GRAPH_SCOPE_ITEMS);
                    outcome["overflow"] = json!(overflow);
                }
                let stopped = server.stop_and_wait().await;
                assert!(stopped.fully_stopped && stopped.errors.is_empty());
            }
            if phase == "fault" && store.ledger_admission_closed().is_some() {
                let second = orch
                    .create_work(
                        &auth,
                        "a-later-create",
                        session,
                        &workspace,
                        "test".into(),
                        "must not admit after an uncertain commit".into(),
                        0,
                        None,
                        None,
                        Vec::new(),
                        WorkPolicy::default(),
                    )
                    .await
                    .expect_err("closed admission must refuse another creation");
                outcome["laterError"] = json!(second);
            }
            write_json(root.join("outcome.json"), &outcome);
            assert_ne!(phase, "create", "configured creation crash was not reached");
            let stopped = orch.stop_background_tasks().await;
            assert!(stopped.fully_stopped && stopped.errors.is_empty());
            drop(orch);
            let shutdown = host.shutdown().await;
            assert!(shutdown.is_clean(), "{}", shutdown.operator_summary());
        });
}

#[test]
fn recovered_creation_at_capacity_remains_pageable_over_authenticated_mcp() {
    let root = tempfile::tempdir().unwrap();
    assert_exit(
        &child_with(
            root.path(),
            "create",
            "create_after_item",
            &[("WORK_CREATION_SEED_COUNT", "4095")],
        ),
        86,
    );
    assert_eq!(
        records(&root.path().join("ledger/work-items")).len(),
        MAX_GRAPH_SCOPE_ITEMS
    );
    let original = read_json(&intent_paths(root.path())[0])["item"].clone();
    assert_exit(&child(root.path(), "read", ""), 0);
    let outcome = read_json(root.path().join("outcome.json"));
    assert_eq!(outcome["response"]["work"], original);
    assert_eq!(outcome["traversedIds"], MAX_GRAPH_SCOPE_ITEMS);
    assert!(outcome["wireMaxBytes"].as_u64().unwrap() <= WORK_PAGE_TRANSPORT_CEILING_BYTES as u64);
    assert!(outcome.get("overflow").is_some());
    println!(
        "Recovered full lane: {} IDs in both list and graph; maximum actual MCP body {} bytes",
        outcome["traversedIds"], outcome["wireMaxBytes"]
    );
    for _ in 0..2 {
        assert_exit(&child(root.path(), "replay", ""), 0);
        assert_eq!(
            read_json(root.path().join("outcome.json"))["response"]["work"],
            original
        );
        assert_eq!(
            records(&root.path().join("ledger/work-items")).len(),
            MAX_GRAPH_SCOPE_ITEMS
        );
    }
}

fn assert_replays(root: &Path, expected: &Value) {
    for reopen in 0..3 {
        assert_exit(&child(root, "replay", ""), 0);
        let outcome = read_json(root.join("outcome.json"));
        assert_eq!(
            outcome.get("response"),
            Some(expected),
            "reopen {reopen}: {outcome}"
        );
        assert!(outcome["admissionClosed"].is_null());
        assert_eq!(
            records(&root.join("ledger/work-items")),
            vec![expected["work"].clone()]
        );
        assert!(records(&root.join("ledger/work-attempts")).is_empty());
        assert!(records(&root.join("ledger/work-decisions")).is_empty());
        assert!(intent_paths(root).is_empty());
        let receipt = read_json(receipt_path(root));
        assert_eq!(receipt["status"], "complete");
        assert_eq!(receipt["response"], *expected);
        assert_eq!(receipt["runId"], expected["work"]["workId"]);
    }
}

#[test]
fn creation_cuts_replay_the_sealed_successor_in_three_fresh_processes() {
    for stage in [
        "lifecycle_after_intent",
        "lifecycle_after_attempts",
        "lifecycle_after_item",
        "lifecycle_after_receipt",
        "lifecycle_after_intent_remove",
        "lifecycle_after_commit",
    ] {
        let root = tempfile::tempdir().unwrap();
        assert_exit(&child(root.path(), "create", stage), 86);
        let intents = intent_paths(root.path());
        let before_item = matches!(stage, "lifecycle_after_intent" | "lifecycle_after_attempts");
        let items = records(&root.path().join("ledger/work-items"));
        assert_eq!(items.len(), usize::from(!before_item), "{stage}");
        let item = if before_item {
            read_json(&intents[0])["item"].clone()
        } else {
            items[0].clone()
        };
        if let Some(path) = intents.first() {
            let intent = read_json(path);
            assert_eq!(intent["schemaVersion"], 2);
            assert!(intent["priorItem"].is_null());
            assert_eq!(intent["request"]["requestId"], REQUEST);
            assert_eq!(
                intent["request"]["ownerId"],
                read_json(root.path().join("fixture.json"))["ownerId"]
            );
            assert_eq!(intent["item"], item);
        }
        let complete = matches!(
            stage,
            "lifecycle_after_receipt" | "lifecycle_after_intent_remove" | "lifecycle_after_commit"
        );
        assert_eq!(
            read_json(receipt_path(root.path()))["status"],
            if complete { "complete" } else { "pending" }
        );
        assert_replays(root.path(), &json!({"work":item,"attempts":[]}));
    }
}

#[test]
fn pre_intent_process_death_has_no_work_effect_and_replays_refusal() {
    let root = tempfile::tempdir().unwrap();
    assert_exit(&child(root.path(), "create", "create_before_intent"), 86);
    assert!(intent_paths(root.path()).is_empty());
    assert!(records(&root.path().join("ledger/work-items")).is_empty());
    for _ in 0..3 {
        assert_exit(&child(root.path(), "replay", ""), 0);
        assert!(read_json(root.path().join("outcome.json"))
            .get("error")
            .is_some());
        assert_eq!(read_json(receipt_path(root.path()))["status"], "failed");
        assert!(records(&root.path().join("ledger/work-items")).is_empty());
    }
}

#[test]
fn creation_write_faults_preserve_admitted_receipts_and_close_live_admission() {
    for stage in ["before_intent", "work", "receipt", "cleanup"] {
        let root = tempfile::tempdir().unwrap();
        assert_exit(&child(root.path(), "fault", stage), 0);
        let outcome = read_json(root.path().join("outcome.json"));
        assert!(outcome.get("error").is_some(), "{stage}: {outcome}");
        let receipt = read_json(receipt_path(root.path()));
        if stage == "before_intent" {
            assert!(outcome["admissionClosed"].is_null());
            assert_eq!(receipt["status"], "failed");
            assert!(intent_paths(root.path()).is_empty());
            assert!(records(&root.path().join("ledger/work-items")).is_empty());
            assert_exit(&child(root.path(), "replay", ""), 0);
            assert_eq!(
                read_json(root.path().join("outcome.json"))["error"],
                outcome["error"]
            );
        } else {
            assert!(outcome["admissionClosed"].is_string());
            assert!(outcome.get("laterError").is_some());
            assert_eq!(
                receipt["status"],
                if stage == "cleanup" {
                    "complete"
                } else {
                    "pending"
                }
            );
            let intents = intent_paths(root.path());
            assert_eq!(intents.len(), 1);
            let item = read_json(&intents[0])["item"].clone();
            assert_replays(root.path(), &json!({"work":item,"attempts":[]}));
        }
    }
}

#[test]
fn creation_recovery_refuses_conflicting_evidence_without_writing_work() {
    for corruption in [
        "snapshot",
        "owner",
        "workspace_digest",
        "payload",
        "failed_receipt",
        "pending_response",
        "pending_error",
        "pending_subject",
        "missing_receipt",
        "corrupt_receipt",
        "schema",
        "filename",
        "foreign_work",
        "substituted_creation_digest",
    ] {
        let root = tempfile::tempdir().unwrap();
        assert_exit(&child(root.path(), "create", "lifecycle_after_intent"), 86);
        let path = intent_paths(root.path()).remove(0);
        let receipt_path = receipt_path(root.path());
        let mut intent = read_json(&path);
        let mut receipt = read_json(&receipt_path);
        match corruption {
            "snapshot" => {
                intent["item"]["objective"] = json!("changed after seal");
                write_json(&path, &intent);
            }
            "substituted_creation_digest" => {
                intent["item"]["objective"] = json!("changed after seal");
                intent["creationItemDigest"] = json!(hash_payload(&intent["item"]));
                write_json(&path, &intent);
            }
            "owner" => {
                intent["request"]["ownerId"] = json!("foreign-owner");
                write_json(&path, &intent);
            }
            "workspace_digest" => {
                intent["request"]["workspaceDigest"] = json!("foreign-digest");
                write_json(&path, &intent);
            }
            "payload" => {
                receipt["payloadHash"] = json!("changed-payload");
                write_json(&receipt_path, &receipt);
            }
            "failed_receipt" => {
                receipt["status"] = json!("failed");
                write_json(&receipt_path, &receipt);
            }
            "pending_response" | "pending_error" | "pending_subject" => {
                match corruption {
                    "pending_response" => {
                        receipt["response"] = json!({"foreign":"pending response"})
                    }
                    "pending_error" => {
                        receipt["error"] =
                            json!({"code":"internal","message":"foreign pending error"})
                    }
                    _ => receipt["runId"] = json!("foreign pending subject"),
                }
                write_json(&receipt_path, &receipt);
            }
            "missing_receipt" => std::fs::remove_file(&receipt_path).unwrap(),
            "corrupt_receipt" => std::fs::write(&receipt_path, "{").unwrap(),
            "schema" => {
                intent["schemaVersion"] = json!(1);
                write_json(&path, &intent);
            }
            "filename" => std::fs::rename(&path, path.with_file_name("foreign-name.json")).unwrap(),
            "foreign_work" => {
                let mut foreign = intent["item"].clone();
                foreign["objective"] = json!("foreign writer");
                let path = root.path().join("ledger/work-items").join(format!(
                    "{}.json",
                    safe_id_filename(foreign["workId"].as_str().unwrap()).unwrap()
                ));
                write_json(path, &foreign);
            }
            _ => unreachable!(),
        }
        let evidence = json_paths(&root.path().join("ledger"))
            .into_iter()
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect::<Vec<_>>();
        for _ in 0..3 {
            assert!(
                OrchStore::open(root.path().join("ledger")).is_err(),
                "{corruption} must refuse recovery"
            );
            for (path, bytes) in &evidence {
                assert_eq!(
                    &std::fs::read(path).unwrap(),
                    bytes,
                    "{corruption}: {}",
                    path.display()
                );
            }
            assert_eq!(
                records(&root.path().join("ledger/work-items")).len(),
                usize::from(corruption == "foreign_work")
            );
        }
    }
}

fn spawn_aliased_create_crash(root: &Path, workspace: &Path, stage: &str) -> Output {
    let alias_display = workspace.join(".").display().to_string();
    child_with(
        root,
        "create",
        stage,
        &[("WORK_CREATION_WORKSPACE_ALIAS", alias_display.as_str())],
    )
}

fn aliased_creation_crash_fixture(stage: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
    assert_exit(
        &spawn_aliased_create_crash(root.path(), &workspace, stage),
        86,
    );
    (root, workspace)
}

#[test]
fn aliased_creation_replays_after_intent_cut_without_closing_admission() {
    let (root, workspace) = aliased_creation_crash_fixture("lifecycle_after_intent");
    let _expected = read_json(intent_paths(root.path()).remove(0));
    let alias_display = workspace.join(".").display().to_string();
    for reopen in 0..3 {
        assert_exit(
            &child_with(
                root.path(),
                "replay",
                "",
                &[("WORK_CREATION_WORKSPACE_ALIAS", alias_display.as_str())],
            ),
            0,
        );
        let outcome = read_json(root.path().join("outcome.json"));
        assert!(
            outcome.get("response").is_some(),
            "reopen {reopen}: {outcome}"
        );
        assert!(outcome["admissionClosed"].is_null());
        assert_eq!(
            outcome["response"]["work"]["workspace"],
            workspace.display().to_string()
        );
        assert_eq!(read_json(receipt_path(root.path()))["status"], "complete");
        assert_eq!(records(&root.path().join("ledger/work-items")).len(), 1);
    }
}

#[test]
fn aliased_substituted_creation_digest_refuses_recovery() {
    let (root, _workspace) = aliased_creation_crash_fixture("lifecycle_after_intent");
    let path = intent_paths(root.path()).remove(0);
    let mut intent = read_json(&path);
    intent["item"]["objective"] = json!("changed after seal");
    intent["creationItemDigest"] = json!(hash_payload(&intent["item"]));
    write_json(&path, &intent);
    for _ in 0..3 {
        assert!(
            OrchStore::open(root.path().join("ledger")).is_err(),
            "aliased substituted digest must refuse recovery"
        );
        assert!(records(&root.path().join("ledger/work-items")).is_empty());
    }
}

#[test]
fn substituted_both_digests_with_untouched_receipt_refuses_recovery() {
    let (root, _workspace) = aliased_creation_crash_fixture("lifecycle_after_intent");
    let path = intent_paths(root.path()).remove(0);
    let mut intent = read_json(&path);
    intent["item"]["objective"] = json!("changed after seal");
    intent["creationItemDigest"] = json!(hash_payload(&intent["item"]));
    let request = &intent["request"];
    let caller_payload = json!({
        "sessionId": request["sessionId"],
        "workspace": request["workspaceDisplay"],
        "kind": intent["item"]["kind"],
        "objective": intent["item"]["objective"],
        "priority": intent["item"]["priority"],
        "deadline": intent["item"]["deadline"],
        "parentWorkId": intent["item"]["parentWorkId"],
        "dependencies": intent["item"]["dependencies"],
        "policy": intent["item"]["policy"],
    });
    intent["creationRequestDigest"] = json!(hash_payload(&caller_payload));
    write_json(&path, &intent);
    let receipt = read_json(receipt_path(root.path()));
    assert_ne!(
        intent["creationRequestDigest"], receipt["payloadHash"],
        "receipt must remain bound to the original caller hash"
    );
    for _ in 0..3 {
        assert!(
            OrchStore::open(root.path().join("ledger")).is_err(),
            "recomputed digests must not bypass the sealed receipt hash"
        );
        assert!(records(&root.path().join("ledger/work-items")).is_empty());
    }
}

#[test]
fn legacy_schema2_creation_intent_without_request_digest_reopens() {
    let root = tempfile::tempdir().unwrap();
    assert_exit(&child(root.path(), "create", "lifecycle_after_intent"), 86);
    let path = intent_paths(root.path()).remove(0);
    let mut intent = read_json(&path);
    intent
        .as_object_mut()
        .unwrap()
        .remove("creationRequestDigest");
    write_json(&path, &intent);
    for reopen in 0..3 {
        assert_exit(&child(root.path(), "replay", ""), 0);
        let outcome = read_json(root.path().join("outcome.json"));
        assert!(
            outcome.get("response").is_some(),
            "legacy schema-2 reopen {reopen}: {outcome}"
        );
        assert_eq!(read_json(receipt_path(root.path()))["status"], "complete");
    }
}

#[test]
fn committed_creation_replays_the_original_request_after_process_death() {
    let root = tempfile::tempdir().unwrap();
    assert_exit(&child(root.path(), "create", "create_after_item"), 86);
    let original = records(&root.path().join("ledger/work-items"));
    assert_eq!(
        original.len(),
        1,
        "the crash follows the durable Work write"
    );
    let expected = json!({"work":original[0],"attempts":[]});
    for reopen in 0..3 {
        assert_exit(&child(root.path(), "replay", ""), 0);
        let outcome = read_json(root.path().join("outcome.json"));
        assert_eq!(
            outcome.get("response"),
            Some(&expected),
            "reopen {reopen} must settle the original request, not fail it: {outcome}"
        );
        assert_eq!(records(&root.path().join("ledger/work-items")), original);
        assert!(records(&root.path().join("ledger/work-attempts")).is_empty());
        assert!(records(&root.path().join("ledger/work-decisions")).is_empty());
    }
}

#[cfg(unix)]
#[test]
fn creation_recovery_does_not_follow_receipt_links_or_adopt_conflicting_legacy_evidence() {
    for variant in ["legacy-conflict", "receipt-link"] {
        let root = tempfile::tempdir().unwrap();
        assert_exit(&child(root.path(), "create", "lifecycle_after_intent"), 86);
        let canonical = receipt_path(root.path());
        let evidence = if variant == "receipt-link" {
            let target = root.path().join("foreign-receipt.json");
            std::fs::rename(&canonical, &target).unwrap();
            std::os::unix::fs::symlink(&target, &canonical).unwrap();
            target
        } else {
            let legacy = canonical
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join(canonical.file_name().unwrap());
            let mut value = read_json(&canonical);
            value["payloadHash"] = json!("foreign legacy hash");
            write_json(&canonical, &value);
            std::fs::rename(&canonical, &legacy).unwrap();
            legacy
        };
        let bytes = std::fs::read(&evidence).unwrap();
        let intent = intent_paths(root.path()).remove(0);
        let sealed = std::fs::read(&intent).unwrap();
        for _ in 0..3 {
            assert!(
                OrchStore::open(root.path().join("ledger")).is_err(),
                "{variant} must refuse"
            );
            assert_eq!(std::fs::read(&evidence).unwrap(), bytes);
            assert_eq!(std::fs::read(&intent).unwrap(), sealed);
            if variant == "receipt-link" {
                assert!(std::fs::symlink_metadata(&canonical)
                    .unwrap()
                    .file_type()
                    .is_symlink());
            } else {
                assert!(!canonical.exists());
            }
            assert!(records(&root.path().join("ledger/work-items")).is_empty());
        }
    }
}
