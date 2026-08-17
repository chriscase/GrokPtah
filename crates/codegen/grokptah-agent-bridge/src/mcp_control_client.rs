//! MCP control-plane client for loopback Streamable HTTP / JSON-RPC (#196/#200).
//!
//! Compatibility client for in-tree tests and coordinators that prefer a
//! lightweight Rust API. Independent interop is proven with
//! `@modelcontextprotocol/sdk` (see `tests/mcp_sdk_interop/`).
//!
//! Lifecycle: `initialize` → `notifications/initialized` → `tools/list` /
//! `tools/call`, with Streamable HTTP Accept headers and optional
//! `mcp-session-id`.

use std::pin::Pin;

use bytes::Bytes;
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::events::SessionUpdate;

/// Bound a coordinator-side SSE frame independently from the server body
/// limit. A frame larger than this is rejected before it can grow buffers.
pub const MAX_LIVE_EVENT_FRAME_BYTES: usize = 512 * 1024;

/// Minimal MCP client against GrokPtah control Streamable HTTP.
pub struct McpControlClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    next_id: u64,
    /// MCP session state: false until successful initialize.
    initialized: bool,
    protocol_version: Option<String>,
    session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListedTool {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct CallResult {
    pub structured: Value,
    pub is_error: bool,
    pub raw: Value,
}

/// Exact identity required by the scoped live run-event channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunScope {
    pub session_id: uuid::Uuid,
    pub workspace: String,
    pub run_id: String,
}

/// A typed event notification delivered by the coordinator event stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PtahEventNotification {
    pub session_id: uuid::Uuid,
    pub workspace: String,
    pub run_id: String,
    pub seq: u64,
    pub ts: String,
    pub update: SessionUpdate,
}

/// A typed recovery notification. The coordinator must poll the durable event
/// tool before opening another live stream when this is received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PtahRecoveryNotification {
    pub session_id: uuid::Uuid,
    pub workspace: String,
    pub run_id: String,
    pub after_seq: u64,
    pub reason: String,
    pub poll_tool: String,
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum LiveNotification {
    Event(PtahEventNotification),
    Recovery(PtahRecoveryNotification),
    /// Preserve unknown notifications for forward-compatible coordinators.
    Unknown {
        method: String,
        params: Value,
    },
}

/// One decoded SSE message, including its resumable durable ID when present.
#[derive(Debug, Clone)]
pub struct LiveEventFrame {
    pub sse_id: Option<u64>,
    pub notification: LiveNotification,
}

type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

/// Incremental bounded decoder for the server's JSON-RPC-over-SSE messages.
pub struct McpEventStream {
    scope: RunScope,
    bytes: ByteStream,
    buffer: Vec<u8>,
    last_event_id: Option<u64>,
    max_frame_bytes: usize,
    closed: bool,
}

impl std::fmt::Debug for McpEventStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpEventStream")
            .field("scope", &self.scope)
            .field("last_event_id", &self.last_event_id)
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl McpEventStream {
    /// Return the last durable event sequence delivered by this stream.
    pub fn last_event_id(&self) -> Option<u64> {
        self.last_event_id
    }

    /// Read the next typed notification. A clean server close returns `None`;
    /// a partial or malformed frame is an error and is never silently dropped.
    pub async fn next_notification(&mut self) -> anyhow::Result<Option<LiveEventFrame>> {
        loop {
            while let Some(frame) = self.take_frame()? {
                if let Some(frame) = frame {
                    if let Some(id) = frame.sse_id {
                        if self.last_event_id.is_some_and(|previous| id <= previous) {
                            anyhow::bail!("live MCP event sequence is not strictly increasing");
                        }
                        self.last_event_id = Some(id);
                    }
                    return Ok(Some(frame));
                }
            }

            if self.closed {
                if self.buffer.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!("live MCP stream closed with a partial SSE frame");
            }

            match self.bytes.next().await {
                Some(Ok(chunk)) => {
                    if self.buffer.len().saturating_add(chunk.len()) > self.max_frame_bytes {
                        anyhow::bail!("live MCP SSE frame exceeds {} bytes", self.max_frame_bytes);
                    }
                    self.buffer.extend_from_slice(&chunk);
                }
                Some(Err(error)) => return Err(error.into()),
                None => self.closed = true,
            }
        }
    }

    fn take_frame(&mut self) -> anyhow::Result<Option<Option<LiveEventFrame>>> {
        let Some((end, delimiter_len)) = find_sse_delimiter(&self.buffer) else {
            return Ok(None);
        };
        let frame: Vec<u8> = self.buffer.drain(..end + delimiter_len).collect();
        let frame = decode_sse_frame(&frame, &self.scope)?;
        Ok(Some(frame))
    }
}

fn find_sse_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    if buffer.len() >= 4 {
        for index in 0..=buffer.len() - 4 {
            if buffer[index..index + 4] == *b"\r\n\r\n" {
                return Some((index, 4));
            }
        }
    }
    if buffer.len() >= 2 {
        for index in 0..=buffer.len() - 2 {
            if buffer[index..index + 2] == *b"\n\n" {
                return Some((index, 2));
            }
        }
    }
    None
}

fn decode_sse_frame(frame: &[u8], scope: &RunScope) -> anyhow::Result<Option<LiveEventFrame>> {
    let text = std::str::from_utf8(frame).map_err(|_| anyhow::anyhow!("SSE frame is not UTF-8"))?;
    let mut event_name = None;
    let mut sse_id = None;
    let mut data = Vec::new();

    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => event_name = Some(value.to_string()),
            "id" => {
                sse_id = Some(
                    value
                        .parse::<u64>()
                        .map_err(|_| anyhow::anyhow!("SSE id is not a u64"))?,
                )
            }
            "data" => data.push(value),
            _ => {}
        }
    }

    if data.is_empty() {
        return Ok(None);
    }
    if event_name.as_deref().is_some_and(|name| name != "message") {
        anyhow::bail!("unexpected MCP SSE event type {:?}", event_name);
    }

    let body: Value = serde_json::from_str(&data.join("\n"))?;
    if body.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        anyhow::bail!("live MCP notification is not JSON-RPC 2.0");
    }
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("live MCP message has no method"))?;
    let params = body
        .get("params")
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("live MCP notification has no params"))?;
    let notification = match method {
        "notifications/ptah_event" => {
            let event: PtahEventNotification = serde_json::from_value(params)?;
            validate_scope(scope, &event.session_id, &event.workspace, &event.run_id)?;
            if sse_id != Some(event.seq) {
                anyhow::bail!("SSE id does not match ptah_event seq");
            }
            LiveNotification::Event(event)
        }
        "notifications/ptah_recovery" => {
            let recovery: PtahRecoveryNotification = serde_json::from_value(params)?;
            validate_scope(
                scope,
                &recovery.session_id,
                &recovery.workspace,
                &recovery.run_id,
            )?;
            LiveNotification::Recovery(recovery)
        }
        _ => LiveNotification::Unknown {
            method: method.to_string(),
            params,
        },
    };
    Ok(Some(LiveEventFrame {
        sse_id,
        notification,
    }))
}

fn validate_scope(
    expected: &RunScope,
    session_id: &uuid::Uuid,
    workspace: &str,
    run_id: &str,
) -> anyhow::Result<()> {
    if expected.session_id != *session_id
        || expected.workspace != workspace
        || expected.run_id != run_id
    {
        anyhow::bail!("live MCP notification scope does not match requested run");
    }
    Ok(())
}

impl McpControlClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::new(),
            next_id: 1,
            initialized: false,
            protocol_version: None,
            session_id: None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        id
    }

    async fn rpc(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            // Streamable HTTP Accept (JSON preferred; SSE also allowed).
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-03-26")
            .json(&body);
        if let Some(sid) = &self.session_id {
            req = req.header("mcp-session-id", sid);
        }
        let resp = req.send().await?;
        if let Some(sid) = resp.headers().get("mcp-session-id") {
            if let Ok(s) = sid.to_str() {
                self.session_id = Some(s.to_string());
            }
        }
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("MCP HTTP {status}: {v}");
        }
        if let Some(err) = v.get("error") {
            anyhow::bail!("MCP error: {err}");
        }
        if let Some(ver) = v.get("jsonrpc").and_then(|x| x.as_str()) {
            if ver != "2.0" {
                anyhow::bail!("server jsonrpc version {ver:?}");
            }
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Low-level RPC for negative tests (custom jsonrpc / auth).
    pub async fn rpc_raw(
        &mut self,
        jsonrpc: &str,
        method: &str,
        params: Value,
        with_auth: bool,
    ) -> anyhow::Result<(reqwest::StatusCode, Value)> {
        let id = self.next_id();
        let body = json!({
            "jsonrpc": jsonrpc,
            "id": id,
            "method": method,
            "params": params,
        });
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("Content-Type", "application/json")
            .json(&body);
        if with_auth {
            req = req.header("Authorization", format!("Bearer {}", self.token));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(json!({}));
        Ok((status, v))
    }

    /// MCP initialize handshake (Streamable HTTP session established).
    pub async fn initialize(&mut self) -> anyhow::Result<Value> {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": { "tools": {} },
                    "clientInfo": {
                        "name": "grokptah-mcp-control-client",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        // Notification may return 202 with empty body — tolerate either shape.
        let _ = self.notify("notifications/initialized", json!({})).await;
        self.initialized = true;
        Ok(result)
    }

    /// Fire-and-forget JSON-RPC notification (no id).
    pub async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let body = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut req = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-03-26")
            .json(&body);
        if let Some(sid) = &self.session_id {
            req = req.header("mcp-session-id", sid);
        }
        let resp = req.send().await?;
        if resp.status().as_u16() == 202 || resp.status().is_success() {
            return Ok(());
        }
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("notification {method} failed: {status} {text}");
    }

    /// End Streamable HTTP session (DELETE /mcp).
    pub async fn close_session(&mut self) -> anyhow::Result<()> {
        let Some(sid) = self.session_id.clone() else {
            return Ok(());
        };
        let resp = self
            .http
            .delete(format!("{}/mcp", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("mcp-session-id", sid)
            .send()
            .await?;
        self.session_id = None;
        self.initialized = false;
        if resp.status().as_u16() == 204 || resp.status().is_success() {
            Ok(())
        } else {
            anyhow::bail!("close_session HTTP {}", resp.status())
        }
    }

    /// Open the authenticated, run-scoped live event channel.
    ///
    /// Pass the last delivered durable sequence when reconnecting. The server
    /// replays strictly after that sequence and emits an explicit recovery
    /// notification when the live channel cannot guarantee continuity.
    pub async fn open_event_stream(
        &self,
        scope: RunScope,
        after_seq: Option<u64>,
    ) -> anyhow::Result<McpEventStream> {
        if !self.initialized {
            anyhow::bail!("MCP client not initialized; call initialize() first");
        }
        let transport_session = self
            .session_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("MCP client has no transport session"))?;
        let mut url = reqwest::Url::parse(&format!("{}/mcp", self.base_url))?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("session_id", &scope.session_id.to_string());
            query.append_pair("workspace", &scope.workspace);
            query.append_pair("run_id", &scope.run_id);
        }
        let mut request = self
            .http
            .get(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("mcp-session-id", transport_session)
            .header("Accept", "text/event-stream")
            .header("MCP-Protocol-Version", "2025-03-26");
        if let Some(seq) = after_seq {
            request = request.header("Last-Event-ID", seq.to_string());
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MCP live stream HTTP {status}: {body}");
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("text/event-stream") {
            anyhow::bail!("MCP live stream returned content type {content_type:?}");
        }
        Ok(McpEventStream {
            scope,
            bytes: Box::pin(response.bytes_stream()),
            buffer: Vec::new(),
            last_event_id: after_seq,
            max_frame_bytes: MAX_LIVE_EVENT_FRAME_BYTES,
            closed: false,
        })
    }

    pub async fn list_tools(&mut self) -> anyhow::Result<Vec<ListedTool>> {
        if !self.initialized {
            anyhow::bail!("MCP client not initialized; call initialize() first");
        }
        let result = self.rpc("tools/list", json!({})).await?;
        let arr = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for t in arr {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .ok_or_else(|| anyhow::anyhow!("tool missing name"))?
                .to_string();
            let schema = t
                .get("inputSchema")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("tool {name} missing inputSchema"))?;
            // Protocol-level: schema must be a typed object with additionalProperties control.
            if schema.get("type").and_then(|t| t.as_str()) != Some("object") {
                anyhow::bail!("tool {name} inputSchema.type must be object");
            }
            out.push(ListedTool {
                name,
                description: t
                    .get("description")
                    .and_then(|d| d.as_str())
                    .map(str::to_string),
                input_schema: schema,
            });
        }
        Ok(out)
    }

    /// Call a tool, validating required fields against the listed schema first.
    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> anyhow::Result<CallResult> {
        if !self.initialized {
            anyhow::bail!("MCP client not initialized; call initialize() first");
        }
        let tools = self.list_tools().await?;
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .ok_or_else(|| anyhow::anyhow!("unknown tool {name}"))?;
        validate_args_against_schema(&tool.input_schema, &arguments)?;
        let raw = self
            .rpc(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments,
                }),
            )
            .await?;
        let is_error = raw
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let structured = raw
            .get("structuredContent")
            .cloned()
            .unwrap_or_else(|| raw.clone());
        Ok(CallResult {
            structured,
            is_error,
            raw,
        })
    }
}

/// Client-side required-field check against MCP inputSchema.
fn validate_args_against_schema(schema: &Value, args: &Value) -> anyhow::Result<()> {
    let required = schema
        .get("required")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    let obj = args.as_object();
    for req in required {
        let key = req
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid required entry"))?;
        let present = obj.map(|o| o.contains_key(key)).unwrap_or(false);
        if !present {
            anyhow::bail!("missing required argument {key}");
        }
    }
    if schema.get("additionalProperties").and_then(|v| v.as_bool()) == Some(false) {
        if let (Some(props), Some(obj)) = (
            schema.get("properties").and_then(|p| p.as_object()),
            args.as_object(),
        ) {
            for k in obj.keys() {
                if !props.contains_key(k) {
                    anyhow::bail!("unexpected argument {k}");
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{AgentHost, HostConfig};
    use crate::mcp_control::start_control_server;
    use crate::orchestration::{
        OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
    };
    use crate::{home_override_serial, set_grokptah_home_override};
    use tempfile::tempdir;

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn client_full_mcp_lifecycle_and_schema_gate() {
        let _guard = home_override_serial();
        let home = tempdir().unwrap();
        set_grokptah_home_override(Some(home.path().join(".grokptah")));
        let ws = tempdir().unwrap();
        let host = AgentHost::create(HostConfig {
            always_approve: true,
            ..HostConfig::default()
        });
        let orch = OrchestrationService::new(
            host.clone(),
            host.event_bus(),
            OrchStore::open(home.path().join("orch")).unwrap(),
            OrchestrationConfig {
                bearer_token: "cli-tok".into(),
                allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
                max_concurrent_runs: 2,
                bounds: RunBounds::default(),
            },
        );
        // Shipped path registers bearer for journal scrubbing.
        assert!(host.event_bus().control_secrets_len() >= 1);

        let srv = start_control_server(orch, 0).await.unwrap();
        let mut client = McpControlClient::new(format!("http://{}", srv.addr), "cli-tok");

        assert!(!client.is_initialized());
        assert!(client.list_tools().await.is_err());

        let init = client.initialize().await.unwrap();
        assert!(
            init["protocolVersion"] == "2025-03-26" || init["protocolVersion"] == "2024-11-05",
            "got {}",
            init["protocolVersion"]
        );
        assert!(client.is_initialized());
        assert!(client.session_id().is_some());

        let tools = client.list_tools().await.unwrap();
        assert!(tools.iter().any(|t| t.name == "ptah_get_capacity"));
        let submit = tools.iter().find(|t| t.name == "ptah_submit_task").unwrap();
        assert_eq!(submit.input_schema["additionalProperties"], false);
        assert!(submit.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "request_id"));

        // Client rejects incomplete args before network call would succeed.
        let missing = client
            .call_tool("ptah_submit_task", json!({"prompt": "x"}))
            .await;
        assert!(missing.is_err());

        let cap = client
            .call_tool("ptah_get_capacity", json!({}))
            .await
            .unwrap();
        assert!(!cap.is_error);
        assert!(
            cap.structured.get("maxConcurrentRuns").is_some()
                || cap.raw.get("structuredContent").is_some()
        );

        // Empty / wrong jsonrpc rejected by server.
        let (st, body) = client
            .rpc_raw("", "tools/list", json!({}), true)
            .await
            .unwrap();
        assert!(st.is_client_error() || body.get("error").is_some());
        assert!(body.get("result").and_then(|r| r.get("tools")).is_none());

        srv.stop();
        set_grokptah_home_override(None);
    }

    #[test]
    fn schema_validation_rejects_extra_and_missing() {
        let schema = json!({
            "type": "object",
            "required": ["a"],
            "additionalProperties": false,
            "properties": { "a": {"type": "string"}, "b": {"type": "string"} }
        });
        assert!(validate_args_against_schema(&schema, &json!({"a": "1"})).is_ok());
        assert!(validate_args_against_schema(&schema, &json!({})).is_err());
        assert!(validate_args_against_schema(&schema, &json!({"a":"1","z":1})).is_err());
    }

    #[tokio::test]
    async fn live_decoder_handles_split_frames_and_comments() {
        let scope = RunScope {
            session_id: uuid::Uuid::new_v4(),
            workspace: "/tmp/disposable".into(),
            run_id: "run-1".into(),
        };
        let body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/ptah_event",
            "params": {
                "sessionId": scope.session_id,
                "workspace": scope.workspace,
                "runId": scope.run_id,
                "seq": 7,
                "ts": "2026-08-12T00:00:00Z",
                "update": {
                    "type": "turn_complete",
                    "session_id": scope.session_id,
                    "cancelled": false
                }
            }
        });
        let encoded = format!(": keep-alive\n\nid: 7\nevent: message\ndata: {}\n\n", body);
        let chunks = vec![
            Ok::<Bytes, reqwest::Error>(Bytes::copy_from_slice(&encoded.as_bytes()[..17])),
            Ok::<Bytes, reqwest::Error>(Bytes::copy_from_slice(&encoded.as_bytes()[17..])),
        ];
        let mut stream = McpEventStream {
            scope,
            bytes: Box::pin(futures::stream::iter(chunks)),
            buffer: Vec::new(),
            last_event_id: None,
            max_frame_bytes: MAX_LIVE_EVENT_FRAME_BYTES,
            closed: false,
        };

        let frame = stream.next_notification().await.unwrap().unwrap();
        assert_eq!(frame.sse_id, Some(7));
        assert!(matches!(
            frame.notification,
            LiveNotification::Event(PtahEventNotification {
                update: SessionUpdate::TurnComplete {
                    cancelled: false,
                    ..
                },
                ..
            })
        ));
        assert_eq!(stream.last_event_id(), Some(7));
        assert!(stream.next_notification().await.unwrap().is_none());
    }

    #[test]
    fn live_decoder_rejects_scope_mismatch_and_oversized_frames() {
        let scope = RunScope {
            session_id: uuid::Uuid::new_v4(),
            workspace: "/tmp/disposable".into(),
            run_id: "run-1".into(),
        };
        let recovery_body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/ptah_recovery",
            "params": {
                "sessionId": scope.session_id,
                "workspace": scope.workspace,
                "runId": scope.run_id,
                "afterSeq": 4,
                "reason": "lagged",
                "pollTool": "ptah_get_events"
            }
        });
        let recovery_frame = format!("event: message\ndata: {}\n\n", recovery_body);
        let decoded = decode_sse_frame(recovery_frame.as_bytes(), &scope)
            .unwrap()
            .unwrap();
        assert!(matches!(
            decoded.notification,
            LiveNotification::Recovery(PtahRecoveryNotification { after_seq: 4, .. })
        ));

        let body = json!({
            "jsonrpc": "2.0",
            "method": "notifications/ptah_recovery",
            "params": {
                "sessionId": uuid::Uuid::new_v4(),
                "workspace": scope.workspace,
                "runId": scope.run_id,
                "afterSeq": 4,
                "reason": "lagged",
                "pollTool": "ptah_get_events"
            }
        });
        let frame = format!("event: message\ndata: {}\n\n", body);
        assert!(decode_sse_frame(frame.as_bytes(), &scope).is_err());

        let chunks = vec![Ok::<Bytes, reqwest::Error>(Bytes::from("123456789"))];
        let mut stream = McpEventStream {
            scope,
            bytes: Box::pin(futures::stream::iter(chunks)),
            buffer: Vec::new(),
            last_event_id: None,
            max_frame_bytes: 8,
            closed: false,
        };
        let error = futures::executor::block_on(stream.next_notification()).unwrap_err();
        assert!(error.to_string().contains("exceeds 8 bytes"));
    }
}
