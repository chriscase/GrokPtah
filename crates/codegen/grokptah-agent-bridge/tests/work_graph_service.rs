//! The work-graph authority as the control plane enforces it (#305).
//!
//! `tests/work_graph_authority.rs` states the properties against the durable
//! ledger. This file proves the authority actually runs on the path a caller
//! reaches — before the first durable write, so a refused graph leaves nothing
//! behind and costs the caller nothing but the error.

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    set_grokptah_home_override, start_control_server, AgentHost, HostConfig, McpControlClient,
    SessionKind,
};
use serde_json::json;
use tempfile::tempdir;

use common::ProcessEnvGuard;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn the_control_plane_refuses_a_cross_lane_dependency_without_writing() {
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

    // Two Build lanes, one workspace. This is the shape the scope binding
    // exists for: a workspace check alone would let either lane name the
    // other's work.
    let mine = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(mine.id, workspace.path()).unwrap();
    let theirs = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(theirs.id, workspace.path()).unwrap();

    let store = OrchStore::open(home.path().join("orchestration")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: "work-graph-token-305".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client =
        McpControlClient::new(format!("http://{}", server.addr), "work-graph-token-305");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    let create =
        |request_id: &str, session: uuid::Uuid, objective: &str, deps: serde_json::Value| {
            json!({
                "request_id": request_id,
                "session_id": session,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": objective,
                "dependencies": deps,
            })
        };

    // Lane B owns a work item. Lane A may not name it.
    let sibling = client
        .call_tool(
            "ptah_create_work",
            create(
                "sibling-create",
                theirs.id,
                "the other lane's work",
                json!([]),
            ),
        )
        .await
        .unwrap();
    let sibling_id = sibling.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    let before = store.list_work_items().unwrap().len();
    let denied = client
        .call_tool(
            "ptah_create_work",
            create(
                "cross-lane-create",
                mine.id,
                "depend on a sibling lane",
                json!([{ "workId": sibling_id, "requiredState": "succeeded" }]),
            ),
        )
        .await
        .expect_err("a cross-lane dependency must be refused")
        .to_string();

    // A work id that exists nowhere is refused identically, so the refusal
    // cannot be read as "that work exists, elsewhere". The transport discards
    // server prose, so the two are indistinguishable to the very last byte.
    let unknown = client
        .call_tool(
            "ptah_create_work",
            create(
                "unknown-create",
                mine.id,
                "depend on nothing at all",
                json!([{ "workId": "no-such-work", "requiredState": "succeeded" }]),
            ),
        )
        .await
        .expect_err("an unknown dependency must be refused")
        .to_string();
    assert_eq!(denied, "MCP remote error: invalid_request");
    assert_eq!(
        denied, unknown,
        "an absent dependency and an unobservable one must answer alike"
    );

    assert_eq!(
        store.list_work_items().unwrap().len(),
        before,
        "a refused graph must leave no durable record behind"
    );

    // The refusal is durable, not a partial success: replaying the identical
    // request answers with the identical refusal rather than the record the
    // first attempt did not write. (`create_work` only ever appends a leaf, so
    // it cannot close a ring by itself; the cycle check guards the writers that
    // can -- durable manager plans, and any ledger adopted from elsewhere.)
    let replayed = client
        .call_tool(
            "ptah_create_work",
            create(
                "cross-lane-create",
                mine.id,
                "depend on a sibling lane",
                json!([{ "workId": sibling_id, "requiredState": "succeeded" }]),
            ),
        )
        .await
        .expect_err("a replayed refusal stays a refusal")
        .to_string();
    assert_eq!(replayed, denied);
    assert_eq!(store.list_work_items().unwrap().len(), before);

    // The same shape inside one lane is legal and lands.
    let root = client
        .call_tool(
            "ptah_create_work",
            create("root-create", mine.id, "a root in my own lane", json!([])),
        )
        .await
        .unwrap();
    let root_id = root.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let child = client
        .call_tool(
            "ptah_create_work",
            create(
                "child-create",
                mine.id,
                "depend on my own lane instead",
                json!([{ "workId": root_id, "requiredState": "succeeded" }]),
            ),
        )
        .await
        .expect("an in-lane dependency is legal");
    assert_eq!(
        child.structured["work"]["dependencies"][0]["workId"],
        root_id.as_str()
    );

    // Lane B still cannot see lane A's graph, and lane A cannot read lane B's.
    let mine_list = client
        .call_tool(
            "ptah_list_work",
            json!({ "session_id": mine.id, "workspace": workspace_text }),
        )
        .await
        .unwrap();
    let listed = mine_list.structured["work"].as_array().unwrap();
    assert!(
        listed
            .iter()
            .all(|item| item["workId"] != sibling_id.as_str()),
        "a lane read must not include a sibling lane's work"
    );
    assert_eq!(listed.len(), 2);

    // The public graph read is deliberately a separate, redacted projection:
    // it exposes dependency/state shape without objectives, workspace paths,
    // principals, agents, results, or lease/attempt material.
    let graph = client
        .call_tool(
            "ptah_get_work_graph",
            json!({ "session_id": mine.id, "workspace": workspace_text }),
        )
        .await
        .unwrap();
    let nodes = graph.structured["graph"].as_array().unwrap();
    assert_eq!(nodes.len(), 2);
    assert!(nodes.iter().all(|node| {
        node.get("objective").is_none()
            && node.get("workspace").is_none()
            && node.get("createdBy").is_none()
            && node.get("assignedAgentId").is_none()
            && node.get("result").is_none()
            && node.get("attempts").is_none()
            && node.get("leaseToken").is_none()
    }));
}
