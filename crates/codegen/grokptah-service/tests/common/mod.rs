//! Shared protocol harness for standalone service tests.
//!
//! Starts a real loopback listener against a disposable home and workspace.
//! Tests talk MCP over HTTP and never require model credentials.

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    ContinuationCheckpoint, ContinuationReason, RunAggregates, RunBounds, RunRecord, RunState,
};
use grokptah_agent_bridge::{
    home_override_serial, set_grokptah_home_override, AgentHostHandle, McpControlClient,
    SessionUpdate,
};
use grokptah_service::{start_service, ServiceConfig, ServiceHandle};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

pub const TOKEN: &str = "service-conformance-token-with-enough-entropy";

pub struct ServiceEnv {
    _serial: std::sync::MutexGuard<'static, ()>,
    pub _home: TempDir,
    pub workspace: TempDir,
    pub other_workspace: TempDir,
    previous_offline: Option<std::ffi::OsString>,
}

impl ServiceEnv {
    pub fn new() -> Self {
        let serial = home_override_serial();
        let home = tempfile::tempdir().expect("temp home");
        let workspace = tempfile::tempdir().expect("temp workspace");
        let other_workspace = tempfile::tempdir().expect("second workspace");
        set_grokptah_home_override(Some(home.path().to_path_buf()));
        let previous_offline = std::env::var_os("GROKPTAH_AGENT_OFFLINE");
        std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
        Self {
            _serial: serial,
            _home: home,
            workspace,
            other_workspace,
            previous_offline,
        }
    }

    pub fn workspace_path(&self) -> PathBuf {
        dunce::canonicalize(self.workspace.path()).expect("canonicalize workspace")
    }

    pub fn other_workspace_path(&self) -> PathBuf {
        dunce::canonicalize(self.other_workspace.path()).expect("canonicalize other workspace")
    }
}

impl Drop for ServiceEnv {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        match &self.previous_offline {
            Some(value) => std::env::set_var("GROKPTAH_AGENT_OFFLINE", value),
            None => std::env::remove_var("GROKPTAH_AGENT_OFFLINE"),
        }
    }
}

pub async fn start_isolated(
    _env: &ServiceEnv,
    workspaces: Vec<PathBuf>,
    max_concurrent: usize,
) -> ServiceHandle {
    let config = ServiceConfig::new(
        "127.0.0.1:0".parse().unwrap(),
        TOKEN,
        workspaces,
        false,
        max_concurrent,
        Duration::from_secs(8),
    )
    .expect("valid service config");
    start_service(config).await.expect("start service")
}

pub async fn mcp_client(addr: SocketAddr) -> McpControlClient {
    let mut client = McpControlClient::new(format!("http://{addr}"), TOKEN);
    client.initialize().await.expect("initialize MCP client");
    client
}

pub async fn create_build_session(
    client: &mut McpControlClient,
    workspace: &Path,
    title: &str,
) -> Uuid {
    let created = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": workspace,
                "title": title,
            }),
        )
        .await
        .unwrap_or_else(|error| panic!("create session {title}: {error}"));
    assert!(
        !created.is_error,
        "create session {title}: {:?}",
        created.raw
    );
    Uuid::parse_str(created.structured["sessionId"].as_str().expect("sessionId"))
        .expect("session uuid")
}

pub fn assert_submission_shape(value: &Value) {
    for key in ["runId", "sessionId", "state", "requestId", "executionMode"] {
        assert!(
            value.get(key).is_some(),
            "submission missing {key}: {value}"
        );
    }
    assert!(value["runId"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(value["sessionId"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(value["requestId"].as_str().is_some_and(|id| !id.is_empty()));
    assert!(value["state"].as_str().is_some());
}

pub fn err_text<T: std::fmt::Debug>(result: &Result<T, impl ToString>) -> String {
    match result {
        Ok(value) => format!("unexpected success: {value:?}"),
        Err(error) => error.to_string(),
    }
}

pub fn seed_active_run(
    host: &AgentHostHandle,
    workspace: &Path,
    run_id: &str,
) -> (Uuid, String, String, u64) {
    let session = host.session_new().unwrap();
    host.session_set_cwd(session.id, workspace).unwrap();
    let session_id = session.id;
    let mut agent = host.ensure_session_agent(session_id).unwrap();
    let agent_id = agent.agent_id.clone();
    let now = Utc::now();
    let start_seq = host.event_bus().next_seq();
    let store = host.ensure_orchestration_store().unwrap();
    let mut checkpoint = ContinuationCheckpoint {
        checkpoint_id: format!("{run_id}-checkpoint"),
        agent_id: agent_id.clone(),
        session_id,
        run_id: run_id.to_string(),
        agent_spec_revision: None,
        parent_checkpoint_id: None,
        ordinal: 1,
        workspace: workspace.display().to_string(),
        context_summary: "conformance checkpoint".into(),
        context_hash: String::new(),
        event_seq: start_seq,
        reason: ContinuationReason::TurnCompleted,
        created_at: now,
    };
    checkpoint.context_hash = checkpoint.context_hash_for();
    store.save_checkpoint(&checkpoint).unwrap();
    agent.state = grokptah_agent_bridge::orchestration::AgentState::Active;
    agent.current_run_id = Some(run_id.to_string());
    agent.latest_checkpoint_id = Some(checkpoint.checkpoint_id.clone());
    agent.updated_at = now;
    store.save_agent(&agent).unwrap();
    store
        .save_run(&RunRecord {
            run_id: run_id.to_string(),
            session_id,
            workspace: workspace.display().to_string(),
            request_id: format!("{run_id}-request"),
            client_id: Some("mcp".into()),
            state: RunState::Running,
            agent_id: Some(agent_id.clone()),
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "conformance run".into(),
            start_seq: Some(start_seq),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();
    host.event_bus().publish(SessionUpdate::AgentMessageChunk {
        session_id,
        text: "first conformance event".into(),
    });
    (session_id, run_id.to_string(), agent_id, start_seq)
}

pub fn point_agent_at_new_run(
    host: &AgentHostHandle,
    session_id: Uuid,
    workspace: &Path,
    previous_run_id: &str,
    next_run_id: &str,
) {
    let store = host.ensure_orchestration_store().unwrap();
    if let Some(mut previous) = store.load_run(previous_run_id).unwrap() {
        previous.state = RunState::Cancelled;
        previous.terminal_result = Some("cancelled".into());
        previous.updated_at = Utc::now();
        store.save_run(&previous).unwrap();
    }
    let now = Utc::now();
    store
        .save_run(&RunRecord {
            run_id: next_run_id.to_string(),
            session_id,
            workspace: workspace.display().to_string(),
            request_id: format!("{next_run_id}-request"),
            client_id: Some("mcp".into()),
            state: RunState::Running,
            agent_id: None,
            retry_of: None,
            parent_run_id: Some(previous_run_id.to_string()),
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "replacement live run".into(),
            start_seq: Some(host.event_bus().next_seq()),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();
    let mut agent = host.ensure_session_agent(session_id).unwrap();
    agent.current_run_id = Some(next_run_id.to_string());
    agent.updated_at = now;
    store.save_agent(&agent).unwrap();
}

/// Send a complete MCP JSON-RPC POST and drop the socket before reading.
pub fn post_mcp_and_drop(
    addr: SocketAddr,
    token: &str,
    transport_session: &str,
    body: &Value,
) -> std::io::Result<()> {
    write_mcp_and_drop(addr, token, transport_session, body, None)
}

/// Send headers plus a truncated body, then drop. The mutation must not commit.
pub fn post_mcp_partial_and_drop(
    addr: SocketAddr,
    token: &str,
    transport_session: &str,
    body: &Value,
) -> std::io::Result<()> {
    write_mcp_and_drop(addr, token, transport_session, body, Some(24))
}

fn write_mcp_and_drop(
    addr: SocketAddr,
    token: &str,
    transport_session: &str,
    body: &Value,
    truncate_body: Option<usize>,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(body).expect("encode jsonrpc body");
    let headers = format!(
        "POST /mcp HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n\
         MCP-Protocol-Version: 2025-03-26\r\n\
         mcp-session-id: {transport_session}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n",
        payload.len()
    );
    let mut stream = TcpStream::connect(addr)?;
    stream.write_all(headers.as_bytes())?;
    match truncate_body {
        Some(limit) => stream.write_all(&payload[..limit.min(payload.len())])?,
        None => stream.write_all(&payload)?,
    }
    drop(stream);
    Ok(())
}

pub fn flood_journal(host: &AgentHostHandle, count: usize) {
    let flood_sid = Uuid::new_v4();
    for i in 0..count {
        host.event_bus().publish(SessionUpdate::AgentMessageChunk {
            session_id: flood_sid,
            text: format!("journal-flood-{i}"),
        });
    }
}
