//! Minimal stdio MCP client for Build agent tool dispatch.
//!
//! Not a full MCP host — enough to list tools and call them on enabled
//! stdio servers configured under ~/.grokptah/mcp.json (and project paths).

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::time::timeout;

use crate::discover::{load_mcp_servers, mcp_config_paths, McpConfigFile, McpServerConfig};

#[derive(Debug, Clone)]
pub struct McpToolSpec {
    pub server: String,
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// OpenAI-style function name: `mcp__{server}__{tool}` (sanitized).
pub fn mcp_function_name(server: &str, tool: &str) -> String {
    format!("mcp__{}__{}", sanitize_ident(server), sanitize_ident(tool))
}

/// Parse `mcp__server__tool` back to (server_sanitized, tool_sanitized).
/// Real server/tool names are recovered via the index built at list time.
#[allow(dead_code)] // public helper for callers / tests
pub fn parse_mcp_function_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    rest.split_once("__")
}

fn sanitize_ident(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Discover tools from enabled stdio MCP servers (best-effort, short timeout).
pub async fn list_mcp_tools(project: Option<&std::path::Path>) -> Vec<McpToolSpec> {
    let servers = load_enabled_stdio_servers(project);
    let mut out = Vec::new();
    for s in servers {
        match timeout(Duration::from_secs(10), list_tools_for_server(&s)).await {
            Ok(Ok(tools)) => out.extend(tools),
            Ok(Err(e)) => {
                eprintln!("[grokptah] mcp list {} failed: {e:#}", s.name);
            }
            Err(_) => {
                eprintln!("[grokptah] mcp list {} timed out", s.name);
            }
        }
        // Cap total tools advertised to the model
        if out.len() >= 48 {
            out.truncate(48);
            break;
        }
    }
    out
}

pub async fn call_mcp_tool(
    project: Option<&std::path::Path>,
    server: &str,
    tool: &str,
    arguments: Value,
) -> Result<String> {
    let servers = load_enabled_stdio_servers(project);
    let s = servers
        .into_iter()
        .find(|s| s.name == server || sanitize_ident(&s.name) == server)
        .ok_or_else(|| anyhow!("unknown or disabled MCP server `{server}`"))?;
    timeout(Duration::from_secs(30), call_tool_on_server(&s, tool, arguments))
        .await
        .map_err(|_| anyhow!("mcp call timed out"))?
}

fn load_enabled_stdio_servers(project: Option<&std::path::Path>) -> Vec<McpServerConfig> {
    use crate::discover::{is_project_local_mcp_config, is_project_mcp_trusted};

    let project_trusted = project
        .map(is_project_mcp_trusted)
        .unwrap_or(false);

    let mut out = Vec::new();
    for path in mcp_config_paths(project) {
        // CRITICAL: never spawn from repo-local .mcp.json unless the project
        // root was explicitly trusted. User-global ~/.grokptah/mcp.json is OK.
        if is_project_local_mcp_config(&path) && !project_trusted {
            continue;
        }
        if let Ok(raw) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<McpConfigFile>(&raw) {
                for s in cfg.servers {
                    if !s.enabled {
                        continue;
                    }
                    let transport = s.transport.to_ascii_lowercase();
                    if (transport == "stdio" || transport.is_empty()) && s.command.is_some() {
                        out.push(s);
                    }
                }
            } else if let Ok(list) = serde_json::from_str::<Vec<McpServerConfig>>(&raw) {
                for s in list {
                    if !s.enabled {
                        continue;
                    }
                    let transport = s.transport.to_ascii_lowercase();
                    if (transport == "stdio" || transport.is_empty()) && s.command.is_some() {
                        out.push(s);
                    }
                }
            }
        }
    }
    // Align with discover::load_mcp_servers enabled flags (first file wins there).
    let enabled: HashMap<String, bool> = load_mcp_servers(project)
        .into_iter()
        .map(|s| (s.name, s.enabled && s.status != "disabled"))
        .collect();
    out.retain(|s| enabled.get(&s.name).copied().unwrap_or(s.enabled));
    let mut seen = std::collections::HashSet::new();
    out.retain(|s| seen.insert(s.name.clone()));
    out
}

async fn list_tools_for_server(server: &McpServerConfig) -> Result<Vec<McpToolSpec>> {
    let mut session = McpSession::spawn(server).await?;
    session.initialize().await?;
    let tools = session.tools_list().await?;
    session.shutdown().await;
    Ok(tools
        .into_iter()
        .map(|t| McpToolSpec {
            server: server.name.clone(),
            name: t.name,
            description: t.description,
            input_schema: t.input_schema,
        })
        .collect())
}

async fn call_tool_on_server(
    server: &McpServerConfig,
    tool: &str,
    arguments: Value,
) -> Result<String> {
    let mut session = McpSession::spawn(server).await?;
    session.initialize().await?;
    // Tool name may be sanitized in the wire name — try exact then original listing match
    let result = match session.tools_call(tool, arguments.clone()).await {
        Ok(r) => r,
        Err(e) => {
            // Recover real tool name from tools/list if sanitized
            let listed = session.tools_list().await.unwrap_or_default();
            if let Some(real) = listed.iter().find(|t| sanitize_ident(&t.name) == tool) {
                session.tools_call(&real.name, arguments).await?
            } else {
                return Err(e);
            }
        }
    };
    session.shutdown().await;
    Ok(result)
}

struct ListedTool {
    name: String,
    description: String,
    input_schema: Value,
}

struct McpSession {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_id: u64,
}

impl McpSession {
    async fn spawn(server: &McpServerConfig) -> Result<Self> {
        let cmd = server
            .command
            .as_ref()
            .ok_or_else(|| anyhow!("MCP server {} has no command", server.name))?;
        let mut child = Command::new(cmd)
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn MCP {}", server.name))?;
        let stdin = child.stdin.take().context("mcp stdin")?;
        let stdout = child.stdout.take().context("mcp stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    async fn initialize(&mut self) -> Result<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": { "name": "grokptah", "version": "0.1.0" }
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(())
    }

    async fn tools_list(&mut self) -> Result<Vec<ListedTool>> {
        let v = self.request("tools/list", json!({})).await?;
        let arr = v
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        for t in arr {
            let name = t
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            let description = t
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            let input_schema = t
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}));
            out.push(ListedTool {
                name,
                description,
                input_schema,
            });
        }
        Ok(out)
    }

    async fn tools_call(&mut self, name: &str, arguments: Value) -> Result<String> {
        let v = self
            .request(
                "tools/call",
                json!({
                    "name": name,
                    "arguments": arguments
                }),
            )
            .await?;
        if let Some(content) = v.get("content").and_then(|c| c.as_array()) {
            let mut parts = Vec::new();
            for item in content {
                if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(t.to_string());
                } else {
                    parts.push(item.to_string());
                }
            }
            if !parts.is_empty() {
                return Ok(parts.join("\n"));
            }
        }
        if let Some(err) = v.get("isError").and_then(|b| b.as_bool()) {
            if err {
                return Ok(format!("MCP tool error: {v}"));
            }
        }
        Ok(v.to_string())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_message(&msg).await?;
        let deadline = Duration::from_secs(12);
        let start = std::time::Instant::now();
        while start.elapsed() < deadline {
            let mut line = String::new();
            let n = timeout(Duration::from_secs(4), self.stdout.read_line(&mut line))
                .await
                .map_err(|_| anyhow!("mcp read timeout"))?
                .context("mcp eof")?;
            if n == 0 {
                bail!("mcp closed");
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // Content-Length framed messages: skip header-only lines
            if line.starts_with("Content-Length:") || line.starts_with("content-length:") {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|i| i.as_u64()) == Some(id)
                || v.get("id").and_then(|i| i.as_i64()) == Some(id as i64)
            {
                if let Some(err) = v.get("error") {
                    bail!("mcp error: {err}");
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
        bail!("mcp request timed out for {method}")
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_message(&msg).await
    }

    async fn write_message(&mut self, msg: &Value) -> Result<()> {
        // Prefer newline-delimited JSON (many MCP stdio servers accept it).
        let line = format!("{msg}\n");
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn function_name_roundtrip_shape() {
        let n = mcp_function_name("my-server", "list_things");
        assert_eq!(n, "mcp__my-server__list_things");
        let (s, t) = parse_mcp_function_name(&n).unwrap();
        assert_eq!(s, "my-server");
        assert_eq!(t, "list_things");
    }
}
