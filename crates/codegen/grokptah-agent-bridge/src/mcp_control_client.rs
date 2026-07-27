//! MCP control-plane client for the loopback JSON-RPC transport (#196).
//!
//! Implements the MCP client lifecycle for this server's tools surface:
//! `initialize` → `notifications/initialized` → `tools/list` / `tools/call`.
//! This is the shipped client library used by integration tests (not ad-hoc
//! one-off HTTP posts embedded in test bodies).

use serde_json::{json, Value};

/// Minimal MCP client against GrokPtah control HTTP JSON-RPC.
pub struct McpControlClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    next_id: u64,
    /// MCP session state: false until successful initialize.
    initialized: bool,
    protocol_version: Option<String>,
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

impl McpControlClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::new(),
            next_id: 1,
            initialized: false,
            protocol_version: None,
        }
    }

    pub fn is_initialized(&self) -> bool {
        self.initialized
    }

    pub fn protocol_version(&self) -> Option<&str> {
        self.protocol_version.as_deref()
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
        let resp = self
            .http
            .post(format!("{}/mcp", self.base_url))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("MCP HTTP {status}: {v}");
        }
        if let Some(err) = v.get("error") {
            anyhow::bail!("MCP error: {err}");
        }
        // JSON-RPC responses must declare version 2.0 when present.
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

    /// MCP initialize handshake.
    pub async fn initialize(&mut self) -> anyhow::Result<Value> {
        let result = self
            .rpc(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
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
        // Server acknowledged — send initialized notification (fire-and-forget).
        let _: Result<Value, anyhow::Error> = self
            .rpc("notifications/initialized", json!({}))
            .await
            .or(Ok(Value::Null));
        self.initialized = true;
        Ok(result)
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
    use crate::set_grokptah_home_override;
    use tempfile::tempdir;

    #[tokio::test]
    async fn client_full_mcp_lifecycle_and_schema_gate() {
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
        assert_eq!(init["protocolVersion"], "2024-11-05");
        assert!(client.is_initialized());

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
}
