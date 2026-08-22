//! Process-level standalone `grokptah-service` plus a loopback fake provider.
//!
//! Used only by the always-on Grokbot certification probe. The parent process
//! may be running the in-process lab host; the child is isolated by
//! `GROKPTAH_HOME` and has ambient provider credentials removed.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use tempfile::TempDir;

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
pub const SYNTHETIC_KEY: &str = "test-not-a-secret";
pub const FIXTURE_BYTES: &[u8] = include_bytes!(
    "../../../crates/codegen/grokptah-service/tests/fixtures/always_on_grokbot.json"
);
pub const FIXTURE_SCHEMA: &str = "grokptah.always_on_grokbot_fixture.v1";
const READY_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const STDERR_BOUND: usize = 64 * 1024;
const RECORD_BOUND: usize = 32;
const LIVE_URL_SENTINELS: &[&str] = &["https://api.x.ai", "https://cli-chat-proxy.grok.com"];
const AMBIENT_CREDENTIAL_ENV: &[&str] = &[
    "XAI_API_KEY",
    "XAI_API_BASE",
    "GROKPTAH_TOKEN_COMMAND",
    "GROKPTAH_AGENT_OFFLINE",
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDisposition {
    Scripted,
    Hold,
}

#[derive(Clone, Debug)]
pub struct ProviderRecord {
    pub method: String,
    pub path: String,
    pub auth_present: bool,
    pub auth_scheme: Option<String>,
    pub body_digest: String,
    pub semantic_id: String,
    pub route_ok: bool,
}

struct ProviderState {
    dispositions: Mutex<HashMap<String, ProviderDisposition>>,
    records: Mutex<Vec<ProviderRecord>>,
    accepted: Mutex<HashMap<String, u64>>,
    accepted_signal: Condvar,
    release_set: Mutex<HashSet<String>>,
    release_signal: Condvar,
    posts: AtomicU64,
}

#[derive(Clone)]
pub struct FakeProvider {
    pub base_url: String,
    state: Arc<ProviderState>,
    _join: Arc<thread::JoinHandle<()>>,
}

impl FakeProvider {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(ProviderState {
            dispositions: Mutex::new(HashMap::new()),
            records: Mutex::new(Vec::new()),
            accepted: Mutex::new(HashMap::new()),
            accepted_signal: Condvar::new(),
            release_set: Mutex::new(HashSet::new()),
            release_signal: Condvar::new(),
            posts: AtomicU64::new(0),
        });
        let state_task = Arc::clone(&state);
        let join = thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else {
                    continue;
                };
                let state = Arc::clone(&state_task);
                thread::spawn(move || handle_provider_conn(stream, &state));
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            state,
            _join: Arc::new(join),
        }
    }

    pub fn arm(&self, semantic_id: &str, disposition: ProviderDisposition) {
        self.state
            .dispositions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(semantic_id.to_string(), disposition);
    }

    pub fn wait_accepted(&self, semantic_id: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let mut accepted = self
            .state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if accepted.get(semantic_id).copied().unwrap_or(0) >= 1 {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                bail!("provider never accepted POST {semantic_id}");
            }
            let (guard, _) = self
                .state
                .accepted_signal
                .wait_timeout(accepted, deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            accepted = guard;
        }
    }

    pub fn send_count(&self) -> u64 {
        self.state.posts.load(Ordering::SeqCst)
    }

    pub fn count_for(&self, semantic_id: &str) -> u64 {
        self.state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(semantic_id)
            .copied()
            .unwrap_or(0)
    }

    pub fn records(&self) -> Vec<ProviderRecord> {
        self.state
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn assert_route_and_auth(&self) -> Result<()> {
        for record in self.records() {
            if !record.route_ok {
                bail!(
                    "unexpected provider route {} {}",
                    record.method,
                    record.path
                );
            }
            if record.body_digest.contains(SYNTHETIC_KEY) || record.body_digest.contains(TOKEN) {
                bail!("provider log stored a raw secret");
            }
            if record.method != "POST" {
                continue;
            }
            if !record.auth_present || record.auth_scheme.as_deref() != Some("bearer") {
                bail!(
                    "provider POST {} lacked bearer presence",
                    record.semantic_id
                );
            }
        }
        Ok(())
    }
}

fn handle_provider_conn(mut stream: TcpStream, state: &ProviderState) {
    let Some((head, headers, body)) = read_http_message(&mut stream) else {
        return;
    };
    let (method, path) = split_request_line(&head);
    if method == "GET" {
        let _ = stream.write_all(models_list().as_bytes());
        return;
    }
    if method != "POST" {
        let _ = stream.write_all(
            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        return;
    }
    let auth = auth_presence(&headers);
    let semantic_id = classify_semantic(&body);
    let record = ProviderRecord {
        method: method.clone(),
        path: path.clone(),
        auth_present: auth.0,
        auth_scheme: auth.1,
        body_digest: hash_payload(&Value::String(body.clone())),
        semantic_id: semantic_id.clone(),
        route_ok: path == "/v1/chat/completions",
    };
    state.posts.fetch_add(1, Ordering::SeqCst);
    {
        let mut records = state
            .records
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        records.push(record);
        if records.len() > RECORD_BOUND {
            records.remove(0);
        }
    }
    {
        let mut accepted = state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *accepted.entry(semantic_id.clone()).or_insert(0) += 1;
        state.accepted_signal.notify_all();
    }
    let disposition = state
        .dispositions
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&semantic_id)
        .copied()
        .unwrap_or(ProviderDisposition::Scripted);
    if disposition == ProviderDisposition::Hold {
        let mut released = state
            .release_set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let hold_deadline = Instant::now() + Duration::from_secs(120);
        while !released.contains(&semantic_id) {
            let now = Instant::now();
            if now >= hold_deadline {
                break;
            }
            let (guard, _) = state
                .release_signal
                .wait_timeout(released, hold_deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            released = guard;
        }
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let response = scripted_completion(&body);
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn split_request_line(head: &str) -> (String, String) {
    let mut parts = head.split_whitespace();
    let method = parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = parts
        .next()
        .unwrap_or("/")
        .split('?')
        .next()
        .unwrap_or("/")
        .to_string();
    (method, path)
}

fn auth_presence(headers: &str) -> (bool, Option<String>) {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("authorization") {
            let scheme = value
                .split_whitespace()
                .next()
                .map(|item| item.to_ascii_lowercase());
            return (true, scheme);
        }
    }
    (false, None)
}

fn classify_semantic(body: &str) -> String {
    let current = current_user_text(body);
    let focus = objective_focus(&current);
    let kind = prompt_kind(&current);
    match kind {
        Some("manager-decision") => "manager-decision".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS complete the replacement") => {
            "step-b-fix".into()
        }
        Some("native") if focus.contains("GROKBOT_FORCE_FAIL") => "step-b".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS first native unit") => "step-a".into(),
        _ if focus.contains("Return exactly this JSON envelope") => "manager-decision".into(),
        _ if current.contains("GROKBOT_SETUP") => "setup".into(),
        _ => "other".into(),
    }
}

fn prompt_kind(text: &str) -> Option<&str> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("Objective:") || line.starts_with("Relevant messages:") {
            break;
        }
        if let Some(kind) = line.strip_prefix("Kind: ").map(str::trim) {
            if matches!(kind, "native" | "manager-decision") {
                return Some(kind);
            }
        }
    }
    None
}

fn objective_focus(text: &str) -> String {
    if let Some((_, rest)) = text.split_once("Objective:\n") {
        let mut lines = Vec::new();
        for line in rest.lines() {
            if line.starts_with("Verified continuation") || line.starts_with("Relevant messages:") {
                break;
            }
            lines.push(line);
        }
        let focused = lines.join("\n");
        if !focused.trim().is_empty() {
            return focused;
        }
    }
    text.to_string()
}

fn message_text(message: &Value) -> Option<String> {
    match message.get("content") {
        Some(Value::String(text)) => Some(text.clone()),
        Some(Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            if out.is_empty() {
                None
            } else {
                Some(out)
            }
        }
        _ => None,
    }
}

fn current_user_text(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    for message in messages.iter().rev() {
        if message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            != "user"
        {
            continue;
        }
        if let Some(text) = message_text(message) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    extract_all_text(body)
}

fn read_http_message(stream: &mut TcpStream) -> Option<(String, String, String)> {
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .ok()?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).ok()?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = std::str::from_utf8(&buf[..header_end]).ok()?.to_string();
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
            let body_end = (body_start + len).min(buf.len());
            let body = String::from_utf8_lossy(&buf[body_start..body_end]).into_owned();
            return Some((first, headers, body));
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    }
    None
}

fn models_list() -> String {
    let body = r#"{"object":"list","data":[{"id":"grok-build","object":"model"}]}"#;
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
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

fn rewrite_directive(content: &str) -> String {
    let start = content
        .find("Envelope: ")
        .map(|index| index + "Envelope: ".len())
        .or_else(|| content.find("{\"directive\""))
        .or_else(|| content.find("{\"schemaVersion\""));
    let Some(start) = start else {
        return "{\"error\":\"missing-envelope\"}".into();
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&take_json_object(&content[start..])) else {
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
            "assignedAgentId": agent_id,
            "policy": {
                "bounds": {
                    "maxPromptBytes": 16384,
                    "maxRounds": 4,
                    "maxDurationMs": 45000,
                    "maxTotalTokens": 8000
                },
                "retry": {
                    "maxAttempts": 1,
                    "retryFailed": false,
                    "retryExpired": false,
                    "backoffMs": 0
                },
                "requiresApproval": false,
                "maxConcurrentAttempts": 1
            }
        }]
    });
    value.to_string()
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

fn scripted_completion(body: &str) -> String {
    match classify_semantic(body).as_str() {
        "manager-decision" => sse_ok(&rewrite_directive(&current_user_text(body))),
        "step-b" => {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 16\r\nConnection: close\r\n\r\nprovider-fail-v1".into()
        }
        _ => sse_ok("GROKBOT_OK"),
    }
}

pub struct ProcessService {
    pub addr: String,
    pub workspace: PathBuf,
    pub provider: FakeProvider,
    child: Child,
    bin: PathBuf,
    home: PathBuf,
    stderr: Arc<Mutex<Vec<u8>>>,
    _home: TempDir,
    _workspace: TempDir,
}

impl ProcessService {
    pub fn spawn() -> Result<Self> {
        let bin =
            PathBuf::from(std::env::var("GROKPTAH_SERVICE_BIN").context("GROKPTAH_SERVICE_BIN")?);
        if !bin.is_file() {
            bail!("GROKPTAH_SERVICE_BIN is not a file");
        }
        let provider = FakeProvider::start();
        let home_dir = tempfile::tempdir().context("runtime home")?;
        let workspace_dir = tempfile::tempdir().context("workspace")?;
        let workspace = dunce::canonicalize(workspace_dir.path())?;
        let home = dunce::canonicalize(home_dir.path())?;
        let listen = free_listen()?;
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let child = spawn_service(
            &bin,
            &provider.base_url,
            &home,
            &workspace,
            &listen,
            &stderr,
        )?;
        wait_ready(&listen)?;
        Ok(Self {
            addr: listen,
            workspace,
            provider,
            child,
            bin,
            home,
            stderr,
            _home: home_dir,
            _workspace: workspace_dir,
        })
    }

    pub fn send_count(&self) -> u64 {
        self.provider.send_count()
    }

    pub fn stderr_text(&self) -> String {
        let bytes = self
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub fn kill_sigkill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn respawn(&mut self) -> Result<()> {
        self.kill_sigkill();
        let listen = free_listen()?;
        self.child = spawn_service(
            &self.bin,
            &self.provider.base_url,
            &self.home,
            &self.workspace,
            &listen,
            &self.stderr,
        )?;
        wait_ready(&listen)?;
        self.addr = listen;
        Ok(())
    }

    pub fn scan_artifacts(&self) -> Result<()> {
        scan_cert_text("stderr", &self.stderr_text())?;
        scan_home(&self.home)?;
        self.provider.assert_route_and_auth()?;
        Ok(())
    }

    pub async fn client(&self) -> Result<McpControlClient> {
        let mut client = McpControlClient::new(format!("http://{}", self.addr), TOKEN);
        let deadline = Instant::now() + READY_WAIT;
        loop {
            match client.initialize().await {
                Ok(_) => return Ok(client),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(POLL).await;
                }
                Err(error) => return Err(error).context("initialize MCP"),
            }
        }
    }
}

impl Drop for ProcessService {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_service(
    bin: &Path,
    provider_base: &str,
    home: &Path,
    workspace: &Path,
    listen: &str,
    stderr_buf: &Arc<Mutex<Vec<u8>>>,
) -> Result<Child> {
    let mut command = Command::new(bin);
    for key in AMBIENT_CREDENTIAL_ENV {
        command.env_remove(key);
    }
    let mut child = command
        .env("GROKPTAH_HOME", home)
        .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
        .env("GROKPTAH_SERVICE_LISTEN", listen)
        .env("GROKPTAH_SERVICE_WORKSPACES", workspace)
        .env("GROKPTAH_SERVICE_MAX_CONCURRENT", "4")
        .env("XAI_API_KEY", SYNTHETIC_KEY)
        .env("XAI_API_BASE", provider_base)
        .env_remove("GROKPTAH_AGENT_OFFLINE")
        .env_remove("GROKPTAH_TOKEN_COMMAND")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn grokptah-service")?;
    if let Some(mut stderr) = child.stderr.take() {
        let buf = Arc::clone(stderr_buf);
        thread::spawn(move || {
            let mut tmp = [0u8; 4096];
            while let Ok(n) = stderr.read(&mut tmp) {
                if n == 0 {
                    break;
                }
                let mut held = buf.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                if held.len() >= STDERR_BOUND {
                    continue;
                }
                let take = (STDERR_BOUND - held.len()).min(n);
                held.extend_from_slice(&tmp[..take]);
            }
        });
    }
    Ok(child)
}

fn free_listen() -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let addr = listener.local_addr()?;
    drop(listener);
    Ok(addr.to_string())
}

fn wait_ready(addr: &str) -> Result<()> {
    let deadline = Instant::now() + READY_WAIT;
    let request = format!("GET /ready HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    while Instant::now() < deadline {
        if let Ok(mut stream) = TcpStream::connect(addr) {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
            if stream.write_all(request.as_bytes()).is_ok() {
                let mut buf = String::new();
                let _ = stream.read_to_string(&mut buf);
                if buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200") {
                    return Ok(());
                }
            }
        }
        thread::sleep(POLL);
    }
    bail!("grokptah-service /ready was not reachable")
}

pub fn scan_cert_text(label: &str, text: &str) -> Result<()> {
    for sentinel in [
        TOKEN,
        SYNTHETIC_KEY,
        "XAI_API_KEY=",
        "GROKPTAH_SERVICE_TOKEN=",
        "GROKPTAH_TOKEN_COMMAND=",
    ] {
        if text.contains(sentinel) {
            bail!("{label} leaked sentinel {sentinel}");
        }
    }
    for sentinel in LIVE_URL_SENTINELS {
        if text.contains(sentinel) {
            bail!("{label} leaked live URL sentinel");
        }
    }
    Ok(())
}

fn scan_home(path: &Path) -> Result<()> {
    scan_home_inner(path, 0)
}

fn scan_home_inner(path: &Path, depth: usize) -> Result<()> {
    if depth > 8 {
        return Ok(());
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        if meta.as_ref().is_some_and(|item| item.is_dir()) {
            scan_home_inner(&entry.path(), depth + 1)?;
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&bytes) {
            scan_cert_text(&format!("home {}", entry.path().display()), text)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_schema_is_bundled() {
        let value: Value = serde_json::from_slice(FIXTURE_BYTES).unwrap();
        assert_eq!(value["schema"], FIXTURE_SCHEMA);
        assert_eq!(value["schemaVersion"], 1);
        assert_eq!(
            value["proposalOnlyEnforcement"],
            "unverified-pending-pr-352"
        );
        assert_eq!(value["soak24h"], "not-run");
        assert_eq!(value["clock"], "bounded-race-controlled-no-fake-clock-seam");
    }
}
