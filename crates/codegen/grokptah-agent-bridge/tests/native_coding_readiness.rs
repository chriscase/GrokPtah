//! Desktop host and loopback MCP must report one host-authored readiness record.

mod common;

use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    project_native_coding_readiness, save_gateway_config, scan_value_for_forbidden_data,
    set_grokptah_home_override, start_control_server, AgentHost, CapabilitySource, GatewayConfig,
    HostConfig, McpControlClient, ModelCapabilities, ProviderModel, ProviderProfile, CONTROL_TOOLS,
};
use serde_json::{json, Value};
use tempfile::tempdir;

use common::ProcessEnvGuard;

fn setup_home() -> (tempfile::TempDir, ProcessEnvGuard) {
    let mut guard = ProcessEnvGuard::new();
    let directory = tempdir().unwrap();
    let home = directory.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
    (directory, guard)
}

fn seed_declared_coding_profile() {
    let mut model = ProviderModel::unqualified("Team/Code:Cheap");
    model.capabilities = ModelCapabilities {
        chat: true,
        tools: true,
        stream: true,
        source: CapabilitySource::Declared,
        ..ModelCapabilities::default()
    };
    let mut profile = ProviderProfile::openai_compatible(
        "company-gateway",
        "Company gateway",
        "http://127.0.0.1:9/v1",
    );
    profile.upsert_model(model);
    let mut config = GatewayConfig::default();
    config.upsert_profile(profile).unwrap();
    save_gateway_config(&config).unwrap();
}

fn assert_secret_free(value: &Value) {
    scan_value_for_forbidden_data(value).expect("readiness projection must be secret-free");
    let encoded = value.to_string();
    for needle in [
        "sk-",
        "Bearer ",
        "http://",
        "https://",
        "api_key",
        "credential_ref",
        "baseUrl",
        "127.0.0.1",
    ] {
        assert!(
            !encoded.contains(needle),
            "projection leaked {needle}: {encoded}"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn desktop_host_and_loopback_mcp_report_the_same_readiness() {
    let (home, _lock) = setup_home();
    seed_declared_coding_profile();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");

    let desktop =
        serde_json::to_value(host.native_coding_readiness("company-gateway", "Team/Code:Cheap"))
            .unwrap();
    let projected = serde_json::to_value(project_native_coding_readiness(
        "primary",
        "company-gateway",
        "Team/Code:Cheap",
    ))
    .unwrap();
    assert_eq!(desktop, projected);
    assert_eq!(desktop["schema"], "grokptah.native-coding-readiness.v1");
    assert_eq!(desktop["ownerId"], "primary");
    assert_eq!(desktop["qualificationEvidence"], "declared");
    assert_eq!(desktop["execution"]["eligibility"], "credential_missing");
    assert_eq!(desktop["execution"]["permitted"], false);
    assert_eq!(
        desktop["managerProposal"]["eligibility"],
        "credential_missing"
    );
    assert_eq!(desktop["computerUse"]["enabled"], false);
    assert_secret_free(&desktop);

    let workspace = tempdir().unwrap();
    host.set_project_cwd(workspace.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "readiness-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let primary = orch.auth_header(Some("Bearer readiness-token")).unwrap();
    let from_orch = orch
        .get_native_coding_readiness(&primary, Some("company-gateway"), Some("Team/Code:Cheap"))
        .unwrap();
    assert_eq!(from_orch, desktop);

    orch.set_agent_owner_id("owner-b".into()).unwrap();
    let other_auth = orch.auth_header(Some("Bearer readiness-token")).unwrap();
    let other_owner = orch
        .get_native_coding_readiness(
            &other_auth,
            Some("company-gateway"),
            Some("Team/Code:Cheap"),
        )
        .unwrap();
    assert_eq!(other_owner["ownerId"], "owner-b");
    assert_eq!(other_owner["execution"], desktop["execution"]);
    assert!(!other_owner.to_string().contains("primary"));
    assert_secret_free(&other_owner);
    orch.set_agent_owner_id("primary".into()).unwrap();

    let server = start_control_server(orch.clone(), 0).await.unwrap();
    let mut client = McpControlClient::new(format!("http://{}", server.addr), "readiness-token");
    client.initialize().await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "ptah_get_native_coding_readiness"),
        "control plane must advertise ptah_get_native_coding_readiness"
    );
    assert!(CONTROL_TOOLS.contains(&"ptah_get_native_coding_readiness"));

    let listed = client
        .call_tool(
            "ptah_get_native_coding_readiness",
            json!({
                "providerId": "company-gateway",
                "modelId": "Team/Code:Cheap",
            }),
        )
        .await
        .unwrap();
    assert!(!listed.is_error, "{:?}", listed.raw);
    assert_eq!(listed.structured, desktop);
    assert_secret_free(&listed.structured);

    client.close_session().await.unwrap();
    server.stop_and_wait().await;
    let _ = host.stop();
}
