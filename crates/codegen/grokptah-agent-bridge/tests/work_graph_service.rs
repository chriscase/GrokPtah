//! The work-graph authority as the control plane enforces it (#305).
//!
//! `tests/work_graph_authority.rs` states the properties against the durable
//! ledger. This file proves the authority actually runs on the path a caller
//! reaches — before the first durable write, so a refused graph leaves no work
//! record. The refusal is recorded under the idempotency key, so a replay is
//! refused again rather than answering with a record that was never written.

mod common;

use grokptah_agent_bridge::orchestration::{
    BlockProvenance, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkState,
    WorkspaceAllowlist,
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

/// `ptah_unblock_work` is the control-plane twin of `ptah_block_work`: same
/// identity/session/workspace fence, same revision fence, same payload shape,
/// and the same unknown vs cross-lane answers. The store already had the
/// transition; this is the path a coordinator actually reaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn the_control_plane_unblocks_under_the_same_fences_as_block() {
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

    let tools = client.list_tools().await.unwrap();
    let unblock = tools
        .iter()
        .find(|tool| tool.name == "ptah_unblock_work")
        .expect("ptah_unblock_work must be advertised");
    let block = tools
        .iter()
        .find(|tool| tool.name == "ptah_block_work")
        .expect("ptah_block_work must be advertised");
    assert_eq!(unblock.input_schema, block.input_schema);

    let created = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "unblock-create",
                "session_id": mine.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "held then released",
            }),
        )
        .await
        .unwrap();
    let work_id = created.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    let revision = created.structured["work"]["revision"].as_u64().unwrap();

    let blocked = client
        .call_tool(
            "ptah_block_work",
            json!({
                "request_id": "unblock-block",
                "session_id": mine.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "reason": "stop for review",
                "expected_revision": revision,
            }),
        )
        .await
        .unwrap();
    assert_eq!(blocked.structured["work"]["state"], "blocked");
    assert_eq!(blocked.structured["work"]["blockProvenance"], "manual");
    let blocked_revision = blocked.structured["work"]["revision"].as_u64().unwrap();

    let stale = client
        .call_tool(
            "ptah_unblock_work",
            json!({
                "request_id": "unblock-stale",
                "session_id": mine.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "reason": "stale fence",
                "expected_revision": blocked_revision + 7,
            }),
        )
        .await
        .expect_err("a stale revision fence must refuse the write")
        .to_string();
    assert_eq!(stale, "MCP remote error: stale_version");
    let still_held = store.load_work_item(&work_id).unwrap().unwrap();
    assert_eq!(still_held.state, WorkState::Blocked);
    assert_eq!(still_held.revision, blocked_revision);

    let released = client
        .call_tool(
            "ptah_unblock_work",
            json!({
                "request_id": "unblock-release",
                "session_id": mine.id,
                "workspace": workspace_text,
                "work_id": work_id,
                "reason": "review complete",
                "expected_revision": blocked_revision,
            }),
        )
        .await
        .unwrap();
    assert_eq!(released.structured["work"]["state"], "queued");
    assert!(released.structured["work"].get("blockProvenance").is_none());
    assert!(released.structured["work"].get("blockedReason").is_none());

    let sibling = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "unblock-sibling",
                "session_id": theirs.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "the other lane's work",
            }),
        )
        .await
        .unwrap();
    let sibling_id = sibling.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    let unknown_args = |request_id: &str| {
        json!({
            "request_id": request_id,
            "session_id": mine.id,
            "workspace": workspace_text,
            "work_id": "no-such-work",
            "reason": "probe",
        })
    };
    let cross_args = |request_id: &str| {
        json!({
            "request_id": request_id,
            "session_id": mine.id,
            "workspace": workspace_text,
            "work_id": sibling_id,
            "reason": "probe",
        })
    };

    let unknown_block = client
        .call_tool("ptah_block_work", unknown_args("unknown-block"))
        .await
        .expect_err("unknown work cannot be blocked")
        .to_string();
    let unknown_unblock = client
        .call_tool("ptah_unblock_work", unknown_args("unknown-unblock"))
        .await
        .expect_err("unknown work cannot be unblocked")
        .to_string();
    assert_eq!(unknown_block, "MCP remote error: invalid_request");
    assert_eq!(
        unknown_unblock, unknown_block,
        "unblock must not distinguish an absent work id from block"
    );

    let cross_block = client
        .call_tool("ptah_block_work", cross_args("cross-block"))
        .await
        .expect_err("a sibling lane's work cannot be blocked from this lane")
        .to_string();
    let cross_unblock = client
        .call_tool("ptah_unblock_work", cross_args("cross-unblock"))
        .await
        .expect_err("a sibling lane's work cannot be unblocked from this lane")
        .to_string();
    assert_eq!(
        cross_unblock, cross_block,
        "unblock must not distinguish a sibling's work from block"
    );

    // A derived dependency hold is reconciliation's, not an operator's to
    // release. The control plane must not widen the store's provenance rule.
    let waiting = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "unblock-waiting",
                "session_id": mine.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "waits on the released item",
                "dependencies": [{ "workId": work_id, "requiredState": "succeeded" }],
            }),
        )
        .await
        .unwrap();
    let waiting_id = waiting.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();
    store.reconcile_workloads().unwrap();
    let derived = store.load_work_item(&waiting_id).unwrap().unwrap();
    assert_eq!(derived.block_provenance, Some(BlockProvenance::Derived));
    let denied = client
        .call_tool(
            "ptah_unblock_work",
            json!({
                "request_id": "unblock-derived",
                "session_id": mine.id,
                "workspace": workspace_text,
                "work_id": waiting_id,
                "reason": "impatience",
            }),
        )
        .await
        .expect_err("a dependency hold is not an operator's to release")
        .to_string();
    assert_eq!(denied, "MCP remote error: conflict");
}
