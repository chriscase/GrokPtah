//! Typed MCP control-plane client for the loopback JSON-RPC transport (#196).
//!
//! This is a real client library (initialize / tools/list / tools/call) used by
//! integration tests and any in-process coordinator — not ad-hoc raw HTTP in tests.

use serde_json::{json, Value};

/// Minimal MCP client against GrokPtah control HTTP JSON-RPC.
pub struct McpControlClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
    next_id: u64,
}

#[derive(Debug, Clone)]
pub struct ListedTool {
    pub name: String,
    pub input_schema: Value,
}

impl McpControlClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
            http: reqwest::Client::new(),
            next_id: 1,
        }
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
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("MCP HTTP {status}: {v}");
        }
        if v.get("error").is_some() {
            anyhow::bail!("MCP error: {}", v["error"]);
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// JSON-RPC without forcing a success status (for negative tests).
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
        let mut req = self.http.post(format!("{}/mcp", self.base_url)).json(&body);
        if with_auth {
            req = req.header("Authorization", format!("Bearer {}", self.token));
        }
        let resp = req.send().await?;
        let status = resp.status();
        let v: Value = resp.json().await.unwrap_or(json!({}));
        Ok((status, v))
    }

    pub async fn initialize(&mut self) -> anyhow::Result<Value> {
        self.rpc(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "grokptah-mcp-control-client", "version": env!("CARGO_PKG_VERSION") }
            }),
        )
        .await
    }

    pub async fn list_tools(&mut self) -> anyhow::Result<Vec<ListedTool>> {
        let result = self.rpc("tools/list", json!({})).await?;
        let arr = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(arr
            .into_iter()
            .filter_map(|t| {
                Some(ListedTool {
                    name: t.get("name")?.as_str()?.to_string(),
                    input_schema: t.get("inputSchema").cloned().unwrap_or(json!({})),
                })
            })
            .collect())
    }

    pub async fn call_tool(&mut self, name: &str, arguments: Value) -> anyhow::Result<Value> {
        self.rpc(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments,
            }),
        )
        .await
    }
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
    async fn client_initialize_list_and_reject_bad_version() {
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
        let srv = start_control_server(orch, 0).await.unwrap();
        let mut client = McpControlClient::new(format!("http://{}", srv.addr), "cli-tok");
        client.initialize().await.unwrap();
        let tools = client.list_tools().await.unwrap();
        assert!(tools.iter().any(|t| t.name == "ptah_get_capacity"));
        let submit = tools.iter().find(|t| t.name == "ptah_submit_task").unwrap();
        assert_eq!(submit.input_schema["additionalProperties"], false);
        assert!(submit.input_schema["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "request_id"));

        // Missing / wrong jsonrpc rejected
        let (st, body) = client
            .rpc_raw("", "tools/list", json!({}), true)
            .await
            .unwrap();
        assert!(st.is_client_error() || body.get("error").is_some());
        let (st2, body2) = client
            .rpc_raw("1.0", "tools/list", json!({}), true)
            .await
            .unwrap();
        assert!(st2.is_client_error() || body2.get("error").is_some());
        // Must not be a successful tools list
        assert!(body2.get("result").and_then(|r| r.get("tools")).is_none());

        let cap = client
            .call_tool("ptah_get_capacity", json!({}))
            .await
            .unwrap();
        assert!(cap.get("structuredContent").is_some() || cap.get("content").is_some());

        srv.stop();
        set_grokptah_home_override(None);
    }
}
