//! Process-level harness for the always-on Grokbot campaign.
//!
//! Spawns the shipped `grokptah-service` binary, a loopback fake provider, and
//! an authenticated MCP client. No production crate is modified.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
pub const SYNTHETIC_KEY: &str = "test-not-a-secret";
const READY_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderScript {
    Lifecycle,
    InvalidDirective,
}

pub struct FakeProvider {
    pub base_url: String,
    sends: Arc<AtomicU64>,
    last_bodies: Arc<Mutex<Vec<String>>>,
    _join: thread::JoinHandle<()>,
}

impl FakeProvider {
    pub fn start() -> Self {
        Self::start_with(ProviderScript::Lifecycle)
    }

    pub fn start_with(script: ProviderScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let addr = listener.local_addr().expect("local addr");
        let sends = Arc::new(AtomicU64::new(0));
        let sends_task = Arc::clone(&sends);
        let script = Arc::new(Mutex::new(script));
        let script_task = Arc::clone(&script);
        let last_bodies = Arc::new(Mutex::new(Vec::new()));
        let last_bodies_task = Arc::clone(&last_bodies);
        let join = thread::spawn(move || {
            listener.set_nonblocking(false).ok();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let Some((head, body)) = read_http_message(&mut stream) else {
                    continue;
                };
                if head.starts_with("GET ") {
                    let _ = stream.write_all(models_list().as_bytes());
                    let _ = stream.flush();
                    continue;
                }
                sends_task.fetch_add(1, Ordering::SeqCst);
                {
                    let mut bodies = last_bodies_task.lock().unwrap_or_else(|p| p.into_inner());
                    bodies.push(body.clone());
                    if bodies.len() > 12 {
                        bodies.remove(0);
                    }
                }
                let script = *script_task.lock().unwrap_or_else(|p| p.into_inner());
                let response = scripted_completion(&body, script);
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            sends,
            last_bodies,
            _join: join,
        }
    }

    pub fn send_count(&self) -> u64 {
        self.sends.load(Ordering::SeqCst)
    }

    pub fn last_user_contents(&self) -> Vec<String> {
        self.last_bodies
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|body| extract_user_content(body))
            .collect()
    }
}

fn read_http_message(stream: &mut TcpStream) -> Option<(String, String)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(header_end) = find_double_crlf(&buf) {
            let headers = std::str::from_utf8(&buf[..header_end]).ok()?;
            let first = headers.lines().next().unwrap_or_default().to_string();
            let lower = headers.to_ascii_lowercase();
            let len = lower
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let body_start = header_end + 4;
            while buf.len() < body_start + len {
                let n = stream.read(&mut tmp).ok()?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let body = String::from_utf8_lossy(&buf[body_start..]).into_owned();
            return Some((first, body));
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    }
    None
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn models_list() -> String {
    let body = r#"{"object":"list","data":[{"id":"grok-build","object":"model"}]}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn scripted_completion(body: &str, script: ProviderScript) -> String {
    let all = extract_all_text(body);
    if all.contains("Return exactly this JSON envelope") {
        return match script {
            ProviderScript::InvalidDirective => sse_ok(r#"{"not":"a-valid-manager-directive"}"#),
            ProviderScript::Lifecycle => sse_ok(&rewrite_directive(&all)),
        };
    }
    // Only the live Work objective may force a child failure. The manager-decision
    // snapshot quotes the failed step and must not match this sentinel.
    let content = extract_user_content(body);
    if content.contains("GROKBOT_FORCE_FAIL") {
        return "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 16\r\nConnection: close\r\n\r\nprovider-fail-v1".into();
    }
    sse_ok("GROKBOT_OK")
}

fn extract_all_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    let mut out = String::new();
    for message in messages {
        match message.get("content") {
            Some(Value::String(text)) => {
                out.push_str(text);
                out.push('\n');
            }
            Some(Value::Array(parts)) => {
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            _ => {}
        }
    }
    if out.is_empty() {
        body.to_string()
    } else {
        out
    }
}

fn extract_user_content(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    for message in messages.iter().rev() {
        match message.get("content") {
            Some(Value::String(text)) => return text.clone(),
            Some(Value::Array(parts)) => {
                let mut out = String::new();
                for part in parts {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        out.push_str(text);
                    }
                }
                if !out.is_empty() {
                    return out;
                }
            }
            _ => {}
        }
    }
    body.to_string()
}

fn rewrite_directive(content: &str) -> String {
    let start = content
        .find("Envelope: ")
        .map(|index| index + "Envelope: ".len())
        .or_else(|| content.find("{\"directive\""))
        .or_else(|| content.find("{\"schemaVersion\""));
    let Some(start) = start else {
        return "{\"error\":\"missing-envelope\"}".into();
    };
    let slice = &content[start..];
    let Ok(mut value) = serde_json::from_str::<Value>(&take_json_object(slice)) else {
        return "{\"error\":\"envelope-parse\"}".into();
    };
    let agent_id = value
        .get("managerAgentId")
        .and_then(Value::as_str)
        .unwrap_or("missing-agent")
        .to_string();
    value["directive"] = json!({
        "type": "append_replacement_steps",
        "reason": "controlled child failure; no files changed",
        "replacesStepIds": ["step-b"],
        "steps": [{
            "stepId": "step-b-fix",
            "kind": "native",
            "objective": "GROKBOT_SUCCESS complete the replacement step",
            "priority": 0,
            "dependencies": ["step-a"],
            "assignedAgentId": agent_id
        }]
    });
    value.to_string()
}

fn take_json_object(input: &str) -> String {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escape = false;
    for (i, ch) in input.char_indices() {
        if in_str {
            if escape {
                escape = false;
            } else if ch == '\\' {
                escape = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return input[..=i].to_string();
                }
            }
            _ => {}
        }
    }
    input.to_string()
}

fn sse_ok(content: &str) -> String {
    let escaped = serde_json::to_string(content).expect("escape content");
    let chunk =
        format!("data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":{escaped}}}}}]}}\n\n");
    let done = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":8,\"completion_tokens\":4,\"total_tokens\":12}}\n\n";
    let end = "data: [DONE]\n\n";
    let body = format!("{chunk}{done}{end}");
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

pub struct ServiceProcess {
    pub addr: String,
    child: Child,
    pub home: PathBuf,
    pub workspace: PathBuf,
    _home_dir: TempDir,
    _workspace_dir: TempDir,
}

impl ServiceProcess {
    pub fn spawn(provider_base: &str) -> Self {
        let home_dir = tempfile::tempdir().expect("runtime home");
        let workspace_dir = tempfile::tempdir().expect("workspace");
        let home = dunce::canonicalize(home_dir.path()).expect("canon home");
        let workspace = dunce::canonicalize(workspace_dir.path()).expect("canon workspace");
        let listen = free_listen();
        let child = spawn_service(provider_base, &home, &workspace, &listen);
        wait_http_ready(&listen);
        Self {
            addr: listen,
            child,
            home,
            workspace,
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
        }
    }

    pub fn respawn(&mut self, provider_base: &str) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let listen = free_listen();
        self.child = spawn_service(provider_base, &self.home, &self.workspace, &listen);
        wait_http_ready(&listen);
        self.addr = listen;
    }
}

impl Drop for ServiceProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_service(provider_base: &str, home: &Path, workspace: &Path, listen: &str) -> Child {
    let bin = env!("CARGO_BIN_EXE_grokptah-service");
    let mut child = Command::new(bin)
        .env("GROKPTAH_HOME", home)
        .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
        .env("GROKPTAH_SERVICE_LISTEN", listen)
        .env("GROKPTAH_SERVICE_WORKSPACES", workspace)
        .env("GROKPTAH_SERVICE_MAX_CONCURRENT", "4")
        .env("XAI_API_KEY", SYNTHETIC_KEY)
        .env("XAI_API_BASE", provider_base)
        .env_remove("GROKPTAH_AGENT_OFFLINE")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn grokptah-service");
    if let Some(mut stderr) = child.stderr.take() {
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while stderr.read(&mut buf).ok().is_some_and(|n| n > 0) {}
        });
    }
    child
}

fn free_listen() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral listen");
    let addr = listener.local_addr().expect("ephemeral addr");
    drop(listener);
    addr.to_string()
}

fn wait_http_ready(addr: &str) {
    let deadline = Instant::now() + READY_WAIT;
    let request = format!("GET /ready HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = String::new();
                let _ = stream.read_to_string(&mut buf);
                if buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200") {
                    return;
                }
            }
        }
        thread::sleep(POLL);
    }
    panic!("grokptah-service /ready was not reachable at {addr}");
}

pub async fn mcp(addr: &str) -> McpControlClient {
    let mut client = McpControlClient::new(format!("http://{addr}"), TOKEN);
    let deadline = Instant::now() + READY_WAIT;
    loop {
        match client.initialize().await {
            Ok(_) => return client,
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(POLL).await;
            }
            Err(error) => panic!("initialize MCP: {error}"),
        }
    }
}

pub async fn call(client: &mut McpControlClient, tool: &str, args: Value) -> Value {
    let result = client
        .call_tool(tool, args)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    assert!(!result.is_error, "{tool} error: {:?}", result.raw);
    result.structured
}

pub async fn call_expect_error(client: &mut McpControlClient, tool: &str, args: Value) -> String {
    match client.call_tool(tool, args).await {
        Ok(result) if result.is_error => result.raw.to_string(),
        Ok(result) => panic!("{tool} should fail: {:?}", result.raw),
        Err(error) => error.to_string(),
    }
}

pub async fn poll_json<F>(
    client: &mut McpControlClient,
    tool: &str,
    args: Value,
    mut pred: F,
) -> Value
where
    F: FnMut(&Value) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut last = json!({});
    while Instant::now() < deadline {
        last = call(client, tool, args.clone()).await;
        if pred(&last) {
            return last;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("{tool} predicate never held: {last}");
}

pub fn rid(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cardinality {
    pub provider_sends: u64,
    pub runs: usize,
    pub work_items: usize,
    pub intents: usize,
    pub decisions: usize,
    pub messages: usize,
    pub plan_revision: u64,
    pub quota_reservations: usize,
}

pub async fn snapshot(
    client: &mut McpControlClient,
    session: Uuid,
    workspace: &Path,
    plan_id: &str,
    sends: u64,
) -> Cardinality {
    let ws = workspace.display().to_string();
    let runs = call(
        client,
        "ptah_list_runs",
        json!({ "session_id": session, "workspace": ws }),
    )
    .await;
    let work = call(
        client,
        "ptah_list_work",
        json!({ "session_id": session, "workspace": ws }),
    )
    .await;
    let intents = call(
        client,
        "ptah_list_execution_intents",
        json!({ "session_id": session, "workspace": ws }),
    )
    .await;
    let plan = call(
        client,
        "ptah_get_manager_plan",
        json!({ "session_id": session, "workspace": ws, "plan_id": plan_id }),
    )
    .await;
    let messages = call(
        client,
        "ptah_list_inbox",
        json!({
            "session_id": session,
            "workspace": ws,
            "agent_id": plan["plan"]["managerAgentId"]
        }),
    )
    .await;
    let encoded = format!("{runs}{work}{intents}{plan}{messages}");
    Cardinality {
        provider_sends: sends,
        runs: runs["runs"].as_array().map(|a| a.len()).unwrap_or(0),
        work_items: work
            .get("work")
            .or_else(|| work.get("items"))
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0),
        intents: intents["intents"].as_array().map(|a| a.len()).unwrap_or(0),
        decisions: work_kind_count(&work, "manager-decision"),
        messages: messages
            .get("messages")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0),
        plan_revision: plan
            .pointer("/plan/revision")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        quota_reservations: if encoded.contains("quotaReservation")
            || encoded.contains("quota_ledger")
            || encoded.contains("QuotaLedger")
        {
            1
        } else {
            0
        },
    }
}

pub fn work_kind_count(work: &Value, kind: &str) -> usize {
    work_items(work)
        .iter()
        .filter(|item| item["kind"].as_str() == Some(kind))
        .count()
}

pub fn succeeded_kind_count(work: &Value, kind: &str) -> usize {
    work_items(work)
        .iter()
        .filter(|item| {
            item["kind"].as_str() == Some(kind) && item["state"].as_str() == Some("succeeded")
        })
        .count()
}

pub fn work_items(work: &Value) -> &[Value] {
    work.get("work")
        .or_else(|| work.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn assert_no_secret_leak(value: &Value) {
    let encoded = value.to_string();
    assert!(
        !encoded.contains(SYNTHETIC_KEY),
        "synthetic credential leaked into MCP payload"
    );
    assert!(
        !encoded.contains("XAI_API_KEY"),
        "credential env name leaked"
    );
}

pub fn assert_no_quota_ledger(value: &Value) {
    let encoded = value.to_string();
    assert!(
        !encoded.contains("QuotaLedger") && !encoded.contains("quotaLedger"),
        "quota ledger must be absent at 67e29bd: {value}"
    );
}

static SERIAL: Mutex<()> = Mutex::new(());

pub fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn pending_usage(run: &Value) -> u64 {
    run.pointer("/aggregates/usagePendingRequests")
        .or_else(|| run.pointer("/aggregates/usage_pending_requests"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}
