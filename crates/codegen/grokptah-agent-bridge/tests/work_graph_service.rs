//! End-to-end scope enforcement for dependency declaration through the real
//! MCP control surface (#305).
//!
//! The store-level suite proves the invariant; this proves a real
//! authenticated client cannot reach around it, and that a dependency
//! declaration is not an existence oracle across scopes.

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
async fn dependency_declaration_is_scope_bound_and_reveals_nothing_foreign() {
    let mut env = ProcessEnvGuard::new();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();

    // Two Build lanes in one workspace: distinct sessions, so distinct scopes.
    let first = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(first.id, workspace.path()).unwrap();
    let second = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(second.id, workspace.path()).unwrap();

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orchestration")).unwrap(),
        OrchestrationConfig {
            bearer_token: "work-graph-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "work-graph-token");
    client.initialize().await.unwrap();
    let workspace_text = workspace.path().display().to_string();

    // An anchor owned by the first session.
    let anchor = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "anchor",
                "session_id": first.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "anchor work item"
            }),
        )
        .await
        .unwrap();
    let anchor_id = anchor.structured["work"]["workId"]
        .as_str()
        .unwrap()
        .to_string();

    // The public projection reports the same admission decision the claim path
    // enforces, so a client is never shown a queued item it would be refused.
    assert_eq!(
        anchor.structured["admission"], "admissible",
        "the projection must carry the canonical admission decision"
    );

    // The owning session may depend on it.
    let in_scope = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "in-scope",
                "session_id": first.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on the anchor",
                "dependencies": [{ "workId": anchor_id, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect("an in-scope dependency is accepted");
    let dependent_id = in_scope.structured["work"]["workId"]
        .as_str()
        .expect("work id")
        .to_string();

    // The second session may not, and must not be able to tell the anchor
    // apart from an id that was never issued.
    let foreign = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "foreign",
                "session_id": second.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on another session's work",
                "dependencies": [{ "workId": anchor_id, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect_err("a cross-session dependency must be refused");

    let invented = uuid::Uuid::new_v4().to_string();
    let unknown = client
        .call_tool(
            "ptah_create_work",
            json!({
                "request_id": "unknown",
                "session_id": second.id,
                "workspace": workspace_text,
                "kind": "verification",
                "objective": "depends on an id that was never issued",
                "dependencies": [{ "workId": invented, "requiredState": "succeeded" }]
            }),
        )
        .await
        .expect_err("an unknown dependency must be refused");

    // Compare what a client actually sees. `Debug` on the transport error
    // carries a captured backtrace whose line numbers differ per call site,
    // which is an artifact of this test rather than anything the client is
    // told.
    let foreign_text = foreign.to_string().replace(&anchor_id, "<id>");
    let unknown_text = unknown.to_string().replace(&invented, "<id>");
    assert_eq!(
        foreign_text, unknown_text,
        "a real foreign id and an invented one must be indistinguishable to a client"
    );
    assert!(
        !foreign_text.contains(&anchor_id),
        "the refusal must not echo another scope's work id: {foreign_text}"
    );

    // A dependent that is waiting reports the wait rather than looking ready.
    let dependent = client
        .call_tool(
            "ptah_get_work",
            json!({
                "session_id": first.id,
                "workspace": workspace_text,
                "work_id": dependent_id,
            }),
        )
        .await
        .expect("read the dependent");
    assert_eq!(dependent.structured["admission"], "dependencies_pending");

    server.stop();
    set_grokptah_home_override(None);
}
