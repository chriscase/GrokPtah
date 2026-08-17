//! MCP Computer Run mutation boundary coverage.
//!
//! The fixture registers a backend owner exactly as the desktop does. The
//! test proves that MCP receives a server-derived client identity, delegates
//! through the shared service, returns only the redacted projection, and
//! preserves workspace/version/takeover fences.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, start_control_server, ActionClass, AgentHost,
    ComputerClientIdentity, ComputerError, ComputerErrorCode, ComputerGrantRequest, ComputerRun,
    ComputerRunController, ComputerUseLimits, ComputerUseService, HostConfig, SimulatorBackend,
};
use serde_json::{json, Value};
use tempfile::tempdir;
use uuid::Uuid;

struct FixtureController {
    service: Arc<ComputerUseService>,
    actors: Arc<Mutex<Vec<String>>>,
}

impl FixtureController {
    fn scoped_run(
        &self,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
    ) -> Result<ComputerRun, ComputerError> {
        self.service
            .get_run(run_id)?
            .filter(|run| {
                run.owner_session_id == owner_session_id
                    && run.workspace.as_deref() == Some(workspace)
            })
            .ok_or_else(|| {
                ComputerError::new(
                    ComputerErrorCode::Unauthorized,
                    "computer run is not available to this client scope",
                )
            })
    }

    fn record_actor(&self, client: &ComputerClientIdentity) {
        self.actors.lock().unwrap().push(client.actor_id());
    }
}

#[async_trait]
impl ComputerRunController for FixtureController {
    async fn authorize(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
        grant_request: ComputerGrantRequest,
    ) -> Result<ComputerRun, ComputerError> {
        self.record_actor(client);
        let run = self.scoped_run(owner_session_id, workspace, run_id)?;
        grant_request.validate(run.limits)?;
        self.service.authorize_mcp_client(
            request_id,
            run_id,
            expected_version,
            client.actor_id(),
            grant_request,
        )
    }

    async fn pause(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.record_actor(client);
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service
            .pause(request_id, run_id, expected_version)
            .await
    }

    async fn take_over(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.record_actor(client);
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service
            .take_over(request_id, run_id, expected_version)
            .await
    }

    async fn cancel(
        &self,
        client: &ComputerClientIdentity,
        request_id: &str,
        owner_session_id: Uuid,
        workspace: &str,
        run_id: &str,
        _expected_version: u64,
    ) -> Result<ComputerRun, ComputerError> {
        self.record_actor(client);
        self.scoped_run(owner_session_id, workspace, run_id)?;
        self.service.cancel(request_id, run_id).await
    }
}

async fn rpc(
    client: &reqwest::Client,
    url: &str,
    session: Option<&str>,
    id: u64,
    method: &str,
    params: Value,
) -> reqwest::Response {
    let mut request = client
        .post(url)
        .header("Authorization", "Bearer mutation-token")
        .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
    if let Some(session) = session {
        request = request.header("mcp-session-id", session);
    }
    request.send().await.unwrap()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn computer_mutations_bind_client_identity_and_preserve_fences() {
    let _guard = home_override_serial();
    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));
    let workspace = tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let session = host
        .session_new_kind(grokptah_agent_bridge::SessionKind::Build)
        .unwrap();
    host.session_set_cwd(session.id, workspace.path()).unwrap();

    let store = host.ensure_computer_store().unwrap();
    let service = Arc::new(ComputerUseService::new(
        Arc::new(SimulatorBackend::new()),
        store,
    ));
    let workspace_string =
        grokptah_agent_bridge::canonical_workspace_string(workspace.path()).unwrap();
    let run = service
        .create_run(
            "fixture-create",
            session.id,
            Some(workspace_string.clone()),
            SimulatorBackend::demo_target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
    let controller = Arc::new(FixtureController {
        service,
        actors: Arc::new(Mutex::new(Vec::new())),
    });
    host.set_computer_run_controller(controller.clone());

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "mutation-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let server = start_control_server(orch, 0).await.unwrap();
    let url = format!("http://{}/mcp", server.addr);
    let client = reqwest::Client::new();

    let init = rpc(
        &client,
        &url,
        None,
        1,
        "initialize",
        json!({
            "protocolVersion":"2025-11-25",
            "capabilities":{},
            "clientInfo":{"name":"mutation-client","version":"7.2"}
        }),
    )
    .await;
    assert_eq!(init.status(), reqwest::StatusCode::OK);
    let transport_session = init
        .headers()
        .get("mcp-session-id")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let init_body: Value = init.json().await.unwrap();
    assert_eq!(
        init_body["result"]["serverInfo"]["name"],
        "grokptah-control"
    );
    let initialized = rpc(
        &client,
        &url,
        Some(&transport_session),
        2,
        "notifications/initialized",
        json!({}),
    )
    .await;
    assert!(matches!(
        initialized.status(),
        reqwest::StatusCode::OK | reqwest::StatusCode::ACCEPTED
    ));

    let unauthorized_without_session = rpc(
        &client,
        &url,
        None,
        3,
        "tools/call",
        json!({
            "name":"ptah_authorize_computer_run",
            "arguments":{
                "request_id":"no-transport-session",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":run.version,
                "action_classes":["semantic"],
                "ttl_ms":60000
            }
        }),
    )
    .await;
    assert_eq!(
        unauthorized_without_session.status(),
        reqwest::StatusCode::FORBIDDEN
    );

    let authorized = rpc(
        &client,
        &url,
        Some(&transport_session),
        4,
        "tools/call",
        json!({
            "name":"ptah_authorize_computer_run",
            "arguments":{
                "request_id":"mcp-authorize-1",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":run.version,
                "action_classes":["semantic"],
                "ttl_ms":60000,
                "uses_remaining":1
            }
        }),
    )
    .await;
    assert_eq!(authorized.status(), reqwest::StatusCode::OK);
    let authorized_body: Value = authorized.json().await.unwrap();
    let projection = &authorized_body["result"]["structuredContent"];
    assert_eq!(projection["state"], "ready");
    let actor = projection["grant"]["issuedBy"]["mcp_client"]["client_id"]
        .as_str()
        .unwrap();
    assert!(actor.starts_with("mutation-client@7.2#"));
    assert!(actor.contains(&transport_session));

    let replayed = rpc(
        &client,
        &url,
        Some(&transport_session),
        41,
        "tools/call",
        json!({
            "name":"ptah_authorize_computer_run",
            "arguments":{
                "request_id":"mcp-authorize-1",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":run.version,
                "action_classes":["semantic"],
                "ttl_ms":60000,
                "uses_remaining":1
            }
        }),
    )
    .await;
    let replayed_status = replayed.status();
    let replayed_body: Value = replayed.json().await.unwrap();
    assert_eq!(
        replayed_status,
        reqwest::StatusCode::OK,
        "replay response: {replayed_body}"
    );
    assert_eq!(
        replayed_body["result"]["structuredContent"]["version"],
        projection["version"]
    );
    assert_eq!(
        replayed_body["result"]["structuredContent"]["grant"]["grantId"],
        projection["grant"]["grantId"]
    );

    let ready_version = projection["version"].as_u64().unwrap();
    let paused = rpc(
        &client,
        &url,
        Some(&transport_session),
        5,
        "tools/call",
        json!({
            "name":"ptah_pause_computer_run",
            "arguments":{
                "request_id":"mcp-pause-1",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":ready_version
            }
        }),
    )
    .await;
    assert_eq!(paused.status(), reqwest::StatusCode::OK);
    let paused_body: Value = paused.json().await.unwrap();
    let paused_projection = &paused_body["result"]["structuredContent"];
    assert_eq!(paused_projection["state"], "paused");
    assert_eq!(paused_projection["controlDisposition"], "paused");

    let stale_takeover = rpc(
        &client,
        &url,
        Some(&transport_session),
        6,
        "tools/call",
        json!({
            "name":"ptah_take_over_computer_run",
            "arguments":{
                "request_id":"mcp-takeover-stale",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":ready_version
            }
        }),
    )
    .await;
    assert_eq!(stale_takeover.status(), reqwest::StatusCode::CONFLICT);

    let current_version = paused_projection["version"].as_u64().unwrap();
    let takeover = rpc(
        &client,
        &url,
        Some(&transport_session),
        7,
        "tools/call",
        json!({
            "name":"ptah_take_over_computer_run",
            "arguments":{
                "request_id":"mcp-takeover-1",
                "session_id":session.id,
                "workspace":workspace.path(),
                "run_id":run.run_id,
                "expected_version":current_version
            }
        }),
    )
    .await;
    assert_eq!(takeover.status(), reqwest::StatusCode::OK);
    let takeover_body: Value = takeover.json().await.unwrap();
    assert_eq!(
        takeover_body["result"]["structuredContent"]["controlDisposition"],
        "operator_takeover"
    );

    let actors = controller.actors.lock().unwrap().clone();
    assert_eq!(actors.len(), 5);
    assert!(actors.iter().all(|actor| actor == &actors[0]));

    server.stop();
    set_grokptah_home_override(None);
}

#[test]
fn mcp_grant_request_rejects_raw_input_classes() {
    let error = ComputerGrantRequest {
        action_classes: BTreeSet::from([ActionClass::PointerFallback]),
        ttl_ms: 1_000,
        uses_remaining: Some(1),
    }
    .validate(ComputerUseLimits::default())
    .unwrap_err();
    assert_eq!(error.code, ComputerErrorCode::InvalidRequest);
}
