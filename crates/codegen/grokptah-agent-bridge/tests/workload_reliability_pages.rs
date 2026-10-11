//! Authenticated MCP traversal of the bounded work page contract (#520).
//!
//! These tests drive the embedded control server. A serializer unit test is
//! not the proof.

mod common;

use std::path::Path;

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, encoded_tools_call_response, issue_cursor_for_tests,
    issue_unknown_version_cursor_for_tests, workspace_digest, AssignmentStatus, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, WorkArtifactRef, WorkDependency,
    WorkItem, WorkPageFilter, WorkPageKind, WorkPageReader, WorkPolicy, WorkProgress, WorkResult,
    WorkState, WorkspaceAllowlist, MAX_GRAPH_SCOPE_ITEMS, MAX_WORK_ARTIFACTS,
    MAX_WORK_ARTIFACT_BYTES, MAX_WORK_OBJECTIVE_BYTES, MAX_WORK_PAGE_LIMIT,
    WORK_PAGE_TRANSPORT_CEILING_BYTES,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, ControlServerHandle,
    DurableWriteFault, HostConfig, HostRuntime, McpControlClient, SessionKind, DURABLE_WRITE_FAULT,
};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

struct Lane {
    _env: ProcessEnvGuard,
    _home: tempfile::TempDir,
    workspace: tempfile::TempDir,
    host: HostRuntime,
    store: OrchStore,
    session: Uuid,
    other_session: Uuid,
    workspace_text: String,
    _server: ControlServerHandle,
    client: McpControlClient,
}

async fn boot() -> Lane {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    host.session_set_cwd(other.id, workspace.path()).unwrap();
    let workspace_text = workspace.path().display().to_string();
    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "work-page-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "work-page-token");
    client.initialize().await.unwrap();
    Lane {
        _env: env,
        _home: home,
        workspace,
        host,
        store,
        session: session.id,
        other_session: other.id,
        workspace_text,
        _server: server,
        client,
    }
}

fn plant(store: &OrchStore, item: WorkItem) {
    store.save_work_item_unchecked(&item).unwrap();
}

fn item_at(
    session: Uuid,
    workspace: &str,
    work_id: &str,
    objective: &str,
    priority: i32,
    created_at: chrono::DateTime<chrono::Utc>,
) -> WorkItem {
    let mut item = WorkItem::new_at(
        "test",
        objective,
        session,
        workspace,
        "NEEDLE_OPERATOR_SECRET",
        WorkPolicy::default(),
        created_at,
    )
    .unwrap();
    item.work_id = work_id.into();
    item.priority = priority;
    item
}

async fn walk_ids(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &str,
    limit: usize,
) -> Vec<String> {
    let mut cursor: Option<String> = None;
    let mut ids = Vec::new();
    let mut previous = String::new();
    for _ in 0..300 {
        let mut args = json!({
            "session_id": session,
            "workspace": workspace,
            "limit": limit,
        });
        if let Some(token) = &cursor {
            args["cursor"] = json!(token);
        }
        let page = client
            .call_tool("ptah_list_work_page", args)
            .await
            .expect("page")
            .structured;
        assert_eq!(page["schemaVersion"], 1);
        assert_eq!(page["kind"], "grokptah.work.page.v1");
        assert_eq!(page["order"], "priority_desc_created_at_asc_work_id_asc");
        assert_eq!(page["consistency"], "version_fence");
        let items = page["items"].as_array().cloned().unwrap_or_default();
        let complete = page["complete"].as_bool().unwrap();
        let body = page.to_string();
        assert!(!body.contains("NEEDLE_OPERATOR_SECRET"));
        assert!(!body.contains("NEEDLE_RESULT_SECRET"));
        assert!(!body.contains(workspace));
        if complete {
            assert!(page.get("continuation").is_none() || page["continuation"].is_null());
            ids.extend(
                items
                    .iter()
                    .filter_map(|item| item["workId"].as_str().map(str::to_string)),
            );
            return ids;
        }
        assert!(!items.is_empty(), "nonterminal page was empty");
        let next = page["continuation"].as_str().unwrap().to_string();
        assert_ne!(next, previous);
        assert!(!next.contains(workspace));
        assert!(!next.contains("NEEDLE_OPERATOR_SECRET"));
        previous = next.clone();
        cursor = Some(next);
        ids.extend(
            items
                .iter()
                .filter_map(|item| item["workId"].as_str().map(str::to_string)),
        );
    }
    panic!("page walk did not finish");
}

fn reader(session: Uuid, workspace: &Path) -> WorkPageReader {
    let canonical = canonical_workspace(workspace).unwrap();
    WorkPageReader {
        owner_id: "primary".into(),
        token_id: "primary".into(),
        session_id: session,
        workspace_digest: workspace_digest(&canonical.display().to_string()),
    }
}

async fn raw_tools_call(
    base: &str,
    token: &str,
    session_header: &str,
    id: u64,
    name: &str,
    arguments: Value,
) -> (reqwest::StatusCode, Vec<u8>) {
    let response = reqwest::Client::new()
        .post(format!("{base}/mcp"))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-03-26")
        .header("mcp-session-id", session_header)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap().to_vec();
    (status, bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn bounded_pages_cover_small_lanes_and_reject_bad_continuations() {
    let mut lane = boot().await;
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let empty = walk_ids(&mut lane.client, lane.session, &lane.workspace_text, 2).await;
    assert!(empty.is_empty());

    for index in 0..(MAX_WORK_PAGE_LIMIT + 1) {
        let mut item = item_at(
            lane.session,
            &lane.workspace_text,
            &format!("p{index:04}"),
            "plain",
            0,
            now,
        );
        if index == 0 {
            item.objective = "quote \" slash \\ snow雪".into();
        }
        if index == 1 {
            item.objective = "雪".repeat(40);
            item.priority = 5;
        }
        if index == 2 {
            item.priority = 5;
            item.dependencies = vec![
                WorkDependency {
                    work_id: "p0001".into(),
                    required_state: WorkState::Succeeded,
                },
                WorkDependency {
                    work_id: "foreign-lane-secret".into(),
                    required_state: WorkState::Succeeded,
                },
            ];
        }
        plant(&lane.store, item);
    }
    let foreign = item_at(
        lane.other_session,
        &lane.workspace_text,
        "other-lane",
        "hidden",
        9,
        now,
    );
    plant(&lane.store, foreign);

    let ids = walk_ids(
        &mut lane.client,
        lane.session,
        &lane.workspace_text,
        MAX_WORK_PAGE_LIMIT,
    )
    .await;
    assert_eq!(ids.len(), MAX_WORK_PAGE_LIMIT + 1);
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len()
    );
    assert!(!ids.iter().any(|id| id == "other-lane"));
    assert_eq!(ids[0], "p0001");
    assert!(ids.iter().position(|id| id == "p0000").unwrap() > 0);

    let first = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 1}),
        )
        .await
        .unwrap()
        .structured;
    let again = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 1}),
        )
        .await
        .unwrap()
        .structured;
    // A fresh call seals a new admission instant, so the continuation may differ.
    // The items and completion of an unchanged lane must not.
    assert_eq!(first["items"], again["items"]);
    assert_eq!(first["complete"], again["complete"]);
    assert_eq!(first["complete"], false);

    let graph = lane
        .client
        .call_tool(
            "ptah_get_work_graph_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 1, "cursor": first["continuation"].as_str().unwrap()}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        graph.contains("not valid")
            || graph.contains("invalid_request")
            || graph.contains("restart_required"),
        "{graph}"
    );

    // Limit 1 puts p0001 and its dependent p0002 on different pages. The
    // workload supervisor reconciles every 5s, so this unchanged-lane check
    // stays on two pages and does not race that pass.
    let graph_head = lane
        .client
        .call_tool(
            "ptah_get_work_graph_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 1}),
        )
        .await
        .unwrap()
        .structured;
    assert_eq!(graph_head["nodes"][0]["workId"], "p0001");
    assert_eq!(graph_head["complete"], false);
    let graph_next = lane
        .client
        .call_tool(
            "ptah_get_work_graph_page",
            json!({
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "limit": 1,
                "cursor": graph_head["continuation"].as_str().unwrap(),
            }),
        )
        .await
        .unwrap()
        .structured;
    let node = &graph_next["nodes"][0];
    assert_eq!(node["workId"], "p0002");
    assert_eq!(node["dependencies"], json!(["p0001"]));
    assert_eq!(node["unresolvedDependencies"], 1);
    let graph_body = graph_next.to_string();
    assert!(!graph_body.contains("NEEDLE_OPERATOR_SECRET"));
    assert!(!graph_body.contains("foreign-lane-secret"));
    assert!(!graph_body.contains(&lane.workspace_text));
    let mut graph_cursor = None;
    let mut graph_ids = Vec::new();
    for _ in 0..8 {
        let mut args = json!({
            "session_id": lane.session,
            "workspace": lane.workspace_text,
            "limit": MAX_WORK_PAGE_LIMIT,
        });
        if let Some(token) = &graph_cursor {
            args["cursor"] = json!(token);
        }
        let page = lane
            .client
            .call_tool("ptah_get_work_graph_page", args)
            .await
            .unwrap()
            .structured;
        let body = page.to_string();
        assert!(!body.contains("NEEDLE_OPERATOR_SECRET"));
        assert!(!body.contains("foreign-lane-secret"));
        assert!(!body.contains(&lane.workspace_text));
        graph_ids.extend(
            page["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|node| node["workId"].as_str().map(str::to_string)),
        );
        if page["complete"].as_bool().unwrap() {
            break;
        }
        let next = page["continuation"].as_str().unwrap().to_string();
        assert_ne!(graph_cursor.as_ref(), Some(&next));
        graph_cursor = Some(next);
    }
    assert_eq!(graph_ids.len(), ids.len());

    let anchor = lane.store.load_work_item("p0000").unwrap().unwrap();
    let mac = lane.store.page_mac_key();
    let digest = grokptah_agent_bridge::orchestration::lane_digest(
        &lane
            .store
            .scoped_work_items(lane.session, &lane.workspace_text)
            .unwrap(),
    );
    let current_reader = reader(lane.session, lane.workspace.path());
    let expired = issue_cursor_for_tests(
        &mac,
        &current_reader,
        &WorkPageFilter::default(),
        WorkPageKind::List,
        &digest,
        now,
        &anchor,
        now,
        now - chrono::Duration::seconds(5),
    )
    .unwrap();
    let expired_error = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "cursor": expired}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(expired_error.contains("cursor_expired"), "{expired_error}");

    let mut foreign_reader = current_reader.clone();
    foreign_reader.owner_id = "other-principal".into();
    let foreign_cursor = issue_cursor_for_tests(
        &mac,
        &foreign_reader,
        &WorkPageFilter::default(),
        WorkPageKind::List,
        &digest,
        now,
        &anchor,
        now,
        now + chrono::Duration::minutes(5),
    )
    .unwrap();
    let foreign_error = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "cursor": foreign_cursor}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(foreign_error.contains("invalid_request"), "{foreign_error}");
    assert!(!foreign_error.contains("other-principal"));

    let unknown_version = issue_unknown_version_cursor_for_tests(
        &mac,
        &current_reader,
        &WorkPageFilter::default(),
        WorkPageKind::List,
        &digest,
        now,
        &anchor,
        now,
    )
    .unwrap();
    let version_error = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "cursor": unknown_version}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(version_error.contains("invalid_request"), "{version_error}");

    let tampered = format!("{expired}x");
    let tamper_error = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "cursor": tampered}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(tamper_error.contains("invalid_request"), "{tamper_error}");

    let header = lane.client.session_id().unwrap().to_string();
    let base = format!("http://{}", lane._server.addr);
    let (status, bytes) = raw_tools_call(
        &base,
        "work-page-token",
        &header,
        77,
        "ptah_list_work_page",
        json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 1}),
    )
    .await;
    assert!(status.is_success());
    let parsed: Value = serde_json::from_slice(&bytes).unwrap();
    let estimated =
        encoded_tools_call_response(&parsed["id"], &parsed["result"]["structuredContent"]);
    assert_eq!(
        bytes.len(),
        estimated.len(),
        "estimator drifted from the wire body"
    );
    assert!(bytes.len() <= WORK_PAGE_TRANSPORT_CEILING_BYTES);

    let (_, oversized) = raw_tools_call(
        &base,
        "work-page-token",
        &header,
        78,
        "ptah_list_work_page",
        json!({"session_id": lane.session, "workspace": lane.workspace_text, "cursor": "x".repeat(5000)}),
    )
    .await;
    let oversized_body = String::from_utf8_lossy(&oversized);
    assert!(oversized_body.contains("not valid") || oversized_body.contains("invalid_request"));
    assert!(!oversized_body.contains(&lane.workspace_text));

    let (unknown_status, unknown_body) = raw_tools_call(
        &base,
        "work-page-token",
        &header,
        79,
        "ptah_get_work",
        json!({"session_id": lane.session, "workspace": lane.workspace_text, "work_id": "missing-work"}),
    )
    .await;
    let (foreign_status, foreign_body) = raw_tools_call(
        &base,
        "work-page-token",
        &header,
        80,
        "ptah_get_work",
        json!({"session_id": lane.other_session, "workspace": lane.workspace_text, "work_id": "p0000"}),
    )
    .await;
    assert_eq!(unknown_status, foreign_status);
    let unknown_message = serde_json::from_slice::<Value>(&unknown_body).unwrap()["error"]
        ["message"]
        .as_str()
        .unwrap()
        .to_string();
    let foreign_message = serde_json::from_slice::<Value>(&foreign_body).unwrap()["error"]
        ["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(unknown_message, foreign_message);
    assert!(!unknown_message.contains(&lane.workspace_text));

    let mut bumped = lane.store.load_work_item("p0003").unwrap().unwrap();
    bumped.priority = 9;
    lane.store.save_work_item(&bumped).unwrap();
    let restart = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "cursor": first["continuation"].as_str().unwrap(),
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(restart.contains("restart_required"), "{restart}");

    lane.host.session_archive(lane.session, true).unwrap();
    let archived = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({"session_id": lane.session, "workspace": lane.workspace_text, "limit": 2}),
        )
        .await
        .unwrap()
        .structured;
    assert!(!archived["items"].as_array().unwrap().is_empty());
    let create = lane
        .client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "archived-create",
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "kind": "test",
                "objective": "must not land",
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(!create.contains("complete"), "{create}");
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn legal_ceiling_lane_is_fully_pageable_and_legacy_tools_refuse() {
    let mut lane = boot().await;
    let now = chrono::DateTime::parse_from_rfc3339("2026-02-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let objective = "x".repeat(256);
    for index in 0..MAX_GRAPH_SCOPE_ITEMS {
        plant(
            &lane.store,
            item_at(
                lane.session,
                &lane.workspace_text,
                &format!("w{index:04}"),
                &objective,
                0,
                now,
            ),
        );
    }
    for tool in ["ptah_list_work", "ptah_get_work_graph"] {
        let error = lane
            .client
            .call_tool(
                tool,
                json!({"session_id": lane.session, "workspace": lane.workspace_text}),
            )
            .await
            .expect_err("legacy tool must refuse the ceiling lane")
            .to_string();
        assert_eq!(
            error, "MCP remote error: capacity_exhausted",
            "{tool} -> {error}"
        );
    }
    let ids = walk_ids(
        &mut lane.client,
        lane.session,
        &lane.workspace_text,
        MAX_WORK_PAGE_LIMIT,
    )
    .await;
    assert_eq!(ids.len(), MAX_GRAPH_SCOPE_ITEMS);
    let unique = ids.iter().collect::<std::collections::BTreeSet<_>>().len();
    assert_eq!(unique, MAX_GRAPH_SCOPE_ITEMS);
    assert_eq!(ids.first().map(String::as_str), Some("w0000"));
    assert_eq!(ids.last().map(String::as_str), Some("w4095"));
    let runs_before = std::fs::read_dir(lane.store.root().join("runs"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    let _ = walk_ids(&mut lane.client, lane.session, &lane.workspace_text, 50).await;
    let runs_after = std::fs::read_dir(lane.store.root().join("runs"))
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert_eq!(runs_before, runs_after);
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn oversized_item_detail_is_pageable_when_get_by_id_refuses() {
    let mut lane = boot().await;
    let now = chrono::Utc::now();
    // get-by-id compares the stored workspace to the canonical claimed path.
    // A tempfile display path (`/var/...`) is visible to list matching and
    // invisible to that exact comparison.
    let canonical = canonical_workspace(lane.workspace.path())
        .unwrap()
        .display()
        .to_string();
    let mut item = item_at(lane.session, &canonical, "fat-item", "objective", 0, now);
    let heavy = "\"".repeat(MAX_WORK_ARTIFACT_BYTES);
    item.result = Some(WorkResult {
        summary: "NEEDLE_RESULT_SECRET".into(),
        evidence: vec![heavy.clone(); MAX_WORK_ARTIFACTS],
        artifacts: (0..MAX_WORK_ARTIFACTS)
            .map(|index| WorkArtifactRef {
                artifact_id: format!("a{index:04}"),
                kind: "log".into(),
                label: heavy.clone(),
                digest: None,
                retained_until: None,
            })
            .collect(),
        failure: None,
        cancellation_reason: None,
        completed_at: now,
        verification: None,
        candidate_verification: None,
    });
    item.progress = Some(WorkProgress {
        summary: "x".repeat(MAX_WORK_OBJECTIVE_BYTES.min(1024)),
        percent: Some(1),
        updated_at: now,
    });
    plant(&lane.store, item);
    let get_error = lane
        .client
        .call_tool(
            "ptah_get_work",
            json!({"session_id": lane.session, "workspace": canonical, "work_id": "fat-item"}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        get_error, "MCP remote error: capacity_exhausted",
        "{get_error}"
    );
    let mut offset = 0usize;
    let mut evidence = 0usize;
    let mut artifacts = 0usize;
    for _ in 0..MAX_WORK_ARTIFACTS * 2 + 4 {
        let page = lane
            .client
            .call_tool(
                "ptah_get_work_detail",
                json!({
                    "session_id": lane.session,
                    "workspace": canonical,
                    "work_id": "fat-item",
                    "section": "result",
                    "offset": offset,
                    "limit": 4
                }),
            )
            .await
            .unwrap()
            .structured;
        let body = page["body"].to_string();
        assert!(
            !body.contains(&canonical) && !body.contains(&lane.workspace_text),
            "detail page leaked the workspace path"
        );
        evidence += page["body"]["evidence"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0);
        artifacts += page["body"]["artifacts"]
            .as_array()
            .map(|v| v.len())
            .unwrap_or(0);
        if page["complete"].as_bool().unwrap() {
            break;
        }
        let next = page["nextOffset"].as_u64().unwrap() as usize;
        assert!(next > offset);
        offset = next;
    }
    assert_eq!(evidence, MAX_WORK_ARTIFACTS);
    assert_eq!(artifacts, MAX_WORK_ARTIFACTS);
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn lane_boundaries_filters_utf8_and_foreign_dependencies() {
    let mut lane = boot().await;
    let now = chrono::DateTime::parse_from_rfc3339("2026-04-01T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let empty = walk_ids(&mut lane.client, lane.session, &lane.workspace_text, 1).await;
    assert!(empty.is_empty());

    plant(
        &lane.store,
        item_at(lane.session, &lane.workspace_text, "n0000", "one", 0, now),
    );
    let one = walk_ids(
        &mut lane.client,
        lane.session,
        &lane.workspace_text,
        MAX_WORK_PAGE_LIMIT,
    )
    .await;
    assert_eq!(one, vec!["n0000".to_string()]);

    for index in 1..MAX_WORK_PAGE_LIMIT - 1 {
        plant(
            &lane.store,
            item_at(
                lane.session,
                &lane.workspace_text,
                &format!("n{index:04}"),
                "boundary",
                0,
                now,
            ),
        );
    }
    let below = walk_ids(
        &mut lane.client,
        lane.session,
        &lane.workspace_text,
        MAX_WORK_PAGE_LIMIT,
    )
    .await;
    assert_eq!(below.len(), MAX_WORK_PAGE_LIMIT - 1);
    assert_eq!(below.first().map(String::as_str), Some("n0000"));
    assert_eq!(below.last().map(String::as_str), Some("n0198"));

    plant(
        &lane.store,
        item_at(lane.session, &lane.workspace_text, "n0199", "limit", 0, now),
    );
    let exact = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "limit": MAX_WORK_PAGE_LIMIT
            }),
        )
        .await
        .unwrap()
        .structured;
    assert_eq!(exact["complete"], true);
    assert!(exact.get("continuation").is_none() || exact["continuation"].is_null());
    assert_eq!(
        exact["items"].as_array().unwrap().len(),
        MAX_WORK_PAGE_LIMIT
    );
    assert_eq!(exact["pageCount"], MAX_WORK_PAGE_LIMIT);

    plant(
        &lane.store,
        item_at(
            lane.session,
            &lane.workspace_text,
            "n0200",
            "overflow",
            0,
            now,
        ),
    );
    let overflow = walk_ids(
        &mut lane.client,
        lane.session,
        &lane.workspace_text,
        MAX_WORK_PAGE_LIMIT,
    )
    .await;
    assert_eq!(overflow.len(), MAX_WORK_PAGE_LIMIT + 1);
    assert_eq!(overflow.last().map(String::as_str), Some("n0200"));

    let filtered = lane
        .client
        .call_tool(
            "ptah_list_work_page",
            json!({
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "limit": 10,
                "states": ["succeeded"]
            }),
        )
        .await
        .unwrap()
        .structured;
    assert_eq!(filtered["complete"], true);
    assert_eq!(filtered["items"].as_array().unwrap().len(), 0);
    assert!(filtered.get("continuation").is_none() || filtered["continuation"].is_null());
    assert_eq!(filtered["pageCount"], 0);

    let symbol = "𝄞";
    let objective = symbol.repeat(MAX_WORK_OBJECTIVE_BYTES / symbol.len());
    assert_eq!(objective.len(), MAX_WORK_OBJECTIVE_BYTES);
    let canonical = canonical_workspace(lane.workspace.path())
        .unwrap()
        .display()
        .to_string();
    plant(
        &lane.store,
        item_at(lane.session, &canonical, "utf8-max", &objective, 0, now),
    );
    let detail = lane
        .client
        .call_tool(
            "ptah_get_work_detail",
            json!({
                "session_id": lane.session,
                "workspace": canonical,
                "work_id": "utf8-max",
                "section": "objective"
            }),
        )
        .await
        .unwrap()
        .structured;
    assert_eq!(detail["complete"], true);
    assert_eq!(detail["body"]["objective"], objective);
    assert!(!detail.to_string().contains(&canonical));

    let mut hub = item_at(lane.session, &lane.workspace_text, "hub", "deps", 1, now);
    for index in 0..64 {
        hub.dependencies.push(WorkDependency {
            work_id: format!("n{index:04}"),
            required_state: WorkState::Succeeded,
        });
    }
    for index in 0..64 {
        hub.dependencies.push(WorkDependency {
            work_id: format!("secret-foreign-{index:02}"),
            required_state: WorkState::Succeeded,
        });
    }
    plant(&lane.store, hub);
    let graph = lane
        .client
        .call_tool(
            "ptah_get_work_graph_page",
            json!({
                "session_id": lane.session,
                "workspace": lane.workspace_text,
                "limit": MAX_WORK_PAGE_LIMIT
            }),
        )
        .await
        .unwrap()
        .structured;
    let nodes = graph["nodes"].as_array().unwrap();
    let hub_node = nodes
        .iter()
        .find(|node| node["workId"] == "hub")
        .expect("hub node");
    assert_eq!(hub_node["dependencies"].as_array().unwrap().len(), 64);
    assert_eq!(hub_node["dependencies"][0], "n0000");
    assert_eq!(hub_node["unresolvedDependencies"], 64);
    let rendered = graph.to_string();
    assert!(!rendered.contains("secret-foreign-"));
    assert!(!rendered.contains(&lane.workspace_text));
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn write_fault_makes_the_live_server_unready_until_reopen() {
    struct FaultGuard;
    impl Drop for FaultGuard {
        fn drop(&mut self) {
            *DURABLE_WRITE_FAULT
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
        }
    }
    let _fault_guard = FaultGuard;
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    let home_path = home.path().to_path_buf();
    set_grokptah_home_override(Some(home_path.join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();
    let canonical = canonical_workspace(workspace.path())
        .unwrap()
        .display()
        .to_string();
    let store = OrchStore::open(home_path.join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "work-page-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut item = item_at(
        session.id,
        &canonical,
        "fault-item",
        "fail me",
        0,
        chrono::Utc::now(),
    );
    item.policy.retry.retry_failed = false;
    item.assigned_agent_id = Some("worker-a".into());
    item.assignment_status = AssignmentStatus::Accepted;
    plant(&store, item);
    let claim = store
        .claim_work_with_lease_secret("fault-item", "worker-a", None, "secret")
        .unwrap();
    *DURABLE_WRITE_FAULT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(DurableWriteFault {
        needle: "work-items",
        skips_remaining: 0,
    });
    let failed = store.fail_work(
        "fault-item",
        &claim.attempt.attempt_id,
        &claim.lease_token,
        WorkResult {
            summary: "worker failed".into(),
            evidence: Vec::new(),
            artifacts: Vec::new(),
            failure: Some("boom".into()),
            cancellation_reason: None,
            completed_at: chrono::Utc::now(),
            verification: None,
            candidate_verification: None,
        },
    );
    assert!(failed.is_err());
    let client = reqwest::Client::new();
    let ready_url = format!("http://{}/ready", server.addr);
    let authed = client
        .get(&ready_url)
        .bearer_auth("work-page-token")
        .send()
        .await
        .unwrap();
    assert_eq!(authed.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let authed_body: Value = authed.json().await.unwrap();
    assert_eq!(authed_body["ready"], false);
    assert_eq!(
        authed_body["capacity"]["health"]["ledgerAdmissionClosed"],
        "ledger admission is closed until the store is reopened"
    );
    assert!(!authed_body.to_string().contains(&canonical));
    let open = client.get(&ready_url).send().await.unwrap();
    assert_eq!(open.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    let open_body: Value = open.json().await.unwrap();
    assert_eq!(open_body["ready"], false);
    assert!(open_body.get("capacity").is_none());
    assert!(!open_body.to_string().contains("ledger admission"));
    assert!(!open_body.to_string().contains(&canonical));
    let refused = store
        .claim_work("other-work", "worker-b", None)
        .unwrap_err();
    assert_eq!(
        refused.message,
        "ledger admission is closed until the store is reopened"
    );

    assert!(server.stop_and_wait().await.is_clean());
    drop(store);
    // The host keeps the ledger handle installed by the service. Reopen is
    // refused until that handle, and its file lock, are gone.
    let shutdown = host.shutdown().await;
    assert!(
        shutdown.durable_writes_sealed && shutdown.process_lock_released,
        "runtime shutdown did not release the home: {shutdown:?}"
    );
    drop(host);
    *DURABLE_WRITE_FAULT
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = None;
    let reopened = OrchStore::open(home_path.join("orchestration")).unwrap();
    assert!(reopened.ledger_admission_closed().is_none());
    assert_eq!(
        reopened
            .load_work_item("fault-item")
            .unwrap()
            .unwrap()
            .state,
        WorkState::Failed
    );
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .expect("reacquire the GrokPtah instance lock");
    host.start().unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        reopened,
        OrchestrationConfig {
            bearer_token: "work-page-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let recovered = client
        .get(format!("http://{}/ready", server.addr))
        .bearer_auth("work-page-token")
        .send()
        .await
        .unwrap();
    assert_eq!(recovered.status(), reqwest::StatusCode::OK);
    let recovered_body: Value = recovered.json().await.unwrap();
    assert_eq!(recovered_body["ready"], true);
    assert!(recovered_body["capacity"]["health"]["ledgerAdmissionClosed"].is_null());
    assert!(server.stop_and_wait().await.is_clean());
    set_grokptah_home_override(None);
}
