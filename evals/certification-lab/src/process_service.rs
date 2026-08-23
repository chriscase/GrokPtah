//! Process-level standalone `grokptah-service` plus a loopback fake provider.
//!
//! Used only by the always-on Grokbot certification probe. The parent process
//! may be running the in-process lab host; the child is isolated by
//! `GROKPTAH_HOME` and has ambient provider credentials removed.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::{scan_value_for_forbidden_data, McpControlClient};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::report::{LoopbackProviderObservation, LoopbackProviderRecord};

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
pub const SYNTHETIC_KEY: &str = "test-not-a-secret";
pub const FIXTURE_BYTES: &[u8] = include_bytes!(
    "../../../crates/codegen/grokptah-service/tests/fixtures/always_on_grokbot.json"
);
pub const FIXTURE_SCHEMA: &str = "grokptah.always_on_grokbot_fixture.v1";
const READY_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);
const RECORD_BOUND: usize = 32;
const STDERR_HEAD: usize = 32 * 1024;
const STDERR_TAIL: usize = 32 * 1024;
const MAX_SCAN_DEPTH: u64 = 8;
const MAX_SCAN_FILES: u64 = 4096;
const MAX_SCAN_FILE_BYTES: u64 = 1024 * 1024;
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
    pub auth_accepted: bool,
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
    rejected_auth: AtomicU64,
    stop: AtomicBool,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

pub struct FakeProvider {
    pub base_url: String,
    listen: SocketAddr,
    state: Arc<ProviderState>,
    accept_join: Mutex<Option<JoinHandle<()>>>,
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
            rejected_auth: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            workers: Mutex::new(Vec::new()),
        });
        let state_task = Arc::clone(&state);
        let join = thread::spawn(move || {
            while !state_task.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if state_task.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let worker_state = Arc::clone(&state_task);
                        let worker = thread::spawn({
                            let worker_state = Arc::clone(&worker_state);
                            move || {
                                handle_provider_conn(stream, &worker_state);
                            }
                        });
                        worker_state
                            .workers
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .push(worker);
                    }
                    Err(_) if state_task.stop.load(Ordering::SeqCst) => break,
                    Err(_) => continue,
                }
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            listen: addr,
            state,
            accept_join: Mutex::new(Some(join)),
        }
    }

    pub fn shutdown(&self) {
        self.state.stop.store(true, Ordering::SeqCst);
        self.state.release_signal.notify_all();
        let _ = TcpStream::connect_timeout(&self.listen, Duration::from_millis(200));
        if let Some(join) = self
            .accept_join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            let _ = join.join();
        }
        let workers = std::mem::take(
            &mut *self
                .state
                .workers
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        for worker in workers {
            let _ = worker.join();
        }
    }

    pub fn observation(&self) -> LoopbackProviderObservation {
        LoopbackProviderObservation {
            accepted_posts: self.send_count(),
            rejected_auth: self.state.rejected_auth.load(Ordering::SeqCst),
            records: self
                .records()
                .into_iter()
                .map(|record| LoopbackProviderRecord {
                    method: record.method,
                    path: record.path,
                    semantic_id: record.semantic_id,
                    body_digest: record.body_digest,
                    auth_accepted: record.auth_accepted,
                    route_ok: record.route_ok,
                })
                .collect(),
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
            if record.auth_accepted
                && (!record.auth_present || record.auth_scheme.as_deref() != Some("bearer"))
            {
                bail!(
                    "accepted provider POST {} lacked bearer presence",
                    record.semantic_id
                );
            }
        }
        Ok(())
    }
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.shutdown();
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
    let auth_accepted = expected_bearer_accepted(&headers);
    let semantic_id = classify_semantic(&body);
    let record = ProviderRecord {
        method: method.clone(),
        path: path.clone(),
        auth_present: auth.0,
        auth_scheme: auth.1,
        auth_accepted,
        body_digest: hash_payload(&Value::String(body.clone())),
        semantic_id: semantic_id.clone(),
        route_ok: path == "/v1/chat/completions",
    };
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
    if !auth_accepted {
        state.rejected_auth.fetch_add(1, Ordering::SeqCst);
        let _ = stream.write_all(
            b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 12\r\nConnection: close\r\n\r\nunauthorized",
        );
        return;
    }
    state.posts.fetch_add(1, Ordering::SeqCst);
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
        while !released.contains(&semantic_id) && !state.stop.load(Ordering::SeqCst) {
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

fn expected_bearer_accepted(headers: &str) -> bool {
    for line in headers.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("authorization") {
            continue;
        }
        let mut parts = value.split_whitespace();
        let scheme = parts.next().unwrap_or("");
        let token = parts.next().unwrap_or("");
        return scheme.eq_ignore_ascii_case("bearer")
            && token == SYNTHETIC_KEY
            && parts.next().is_none();
    }
    false
}

fn classify_semantic(body: &str) -> String {
    let current = current_user_text(body);
    let focus = objective_focus(&current);
    let kind = prompt_kind(&current);
    if current.contains("GROKBOT_SETUP") && kind.is_none() {
        return "setup".into();
    }
    match kind {
        Some("manager-decision") => "manager-decision".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS complete the replacement") => {
            "step-b-fix".into()
        }
        Some("native") if focus.contains("GROKBOT_FORCE_FAIL") => "step-b".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS first native unit") => "step-a".into(),
        Some("native") if focus.contains("GROKBOT_SUCCESS") => "native-success".into(),
        Some("native") => "other".into(),
        None if focus.contains("Return exactly this JSON envelope") => "manager-decision".into(),
        None if current.contains("GROKBOT_SETUP") => "setup".into(),
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

struct StderrCapture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
}

impl StderrCapture {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.head.len() < STDERR_HEAD {
                self.head.push(byte);
            } else {
                if self.tail.len() >= STDERR_TAIL {
                    self.tail.pop_front();
                }
                self.tail.push_back(byte);
            }
        }
    }

    fn texts(&self) -> (String, String) {
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        (
            String::from_utf8_lossy(&self.head).into_owned(),
            String::from_utf8_lossy(&tail).into_owned(),
        )
    }
}

pub struct ProcessService {
    pub addr: String,
    pub previous_addr: Option<String>,
    pub previous_pid: Option<u32>,
    pub workspace: PathBuf,
    pub provider: FakeProvider,
    child: Child,
    bin: PathBuf,
    home: PathBuf,
    stderr: Arc<Mutex<StderrCapture>>,
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
        let stderr = empty_stderr();
        let mut child = spawn_service(&bin, &provider.base_url, &home, &workspace, &stderr)?;
        let addr = wait_child_ready(&mut child, &stderr)?;
        Ok(Self {
            addr,
            previous_addr: None,
            previous_pid: None,
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

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn send_count(&self) -> u64 {
        self.provider.send_count()
    }

    pub fn stderr_text(&self) -> String {
        let (head, tail) = self
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .texts();
        format!("{head}{tail}")
    }

    pub fn kill_sigkill(&mut self) {
        self.previous_addr = Some(self.addr.clone());
        self.previous_pid = Some(self.pid());
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn assert_previous_endpoint_dead(&self) -> Result<()> {
        let addr = self
            .previous_addr
            .as_deref()
            .context("kill_sigkill must record the previous listen address")?;
        if !endpoint_dead(addr) {
            bail!("previous MCP endpoint {addr} is still reachable after SIGKILL");
        }
        Ok(())
    }

    pub fn respawn(&mut self) -> Result<()> {
        self.kill_sigkill();
        self.assert_previous_endpoint_dead()?;
        let mut last = String::new();
        for attempt in 1..=5 {
            self.stderr = empty_stderr();
            self.child = spawn_service(
                &self.bin,
                &self.provider.base_url,
                &self.home,
                &self.workspace,
                &self.stderr,
            )?;
            match wait_child_ready_result(&mut self.child, &self.stderr) {
                Ok(listen) => {
                    self.addr = listen;
                    if Some(self.pid()) == self.previous_pid {
                        bail!("respawned service reused the killed PID");
                    }
                    return Ok(());
                }
                Err(error) => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    last = format!("{attempt}: {error}");
                }
            }
        }
        bail!("respawn grokptah-service never became ready: {last}")
    }

    pub fn scan_artifacts(&self) -> Result<()> {
        let (head, tail) = self
            .stderr
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .texts();
        scan_cert_text("stderr-head", &head)?;
        scan_cert_text("stderr-tail", &tail)?;
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
    stderr_buf: &Arc<Mutex<StderrCapture>>,
) -> Result<Child> {
    let mut command = Command::new(bin);
    for key in AMBIENT_CREDENTIAL_ENV {
        command.env_remove(key);
    }
    let mut child = command
        .env("GROKPTAH_HOME", home)
        .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
        .env("GROKPTAH_SERVICE_LISTEN", "127.0.0.1:0")
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
                buf.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(&tmp[..n]);
            }
        });
    }
    Ok(child)
}

fn endpoint_dead(addr: &str) -> bool {
    let Ok(socket) = addr.parse::<SocketAddr>() else {
        return true;
    };
    TcpStream::connect_timeout(&socket, Duration::from_millis(200)).is_err()
}

fn empty_stderr() -> Arc<Mutex<StderrCapture>> {
    Arc::new(Mutex::new(StderrCapture {
        head: Vec::new(),
        tail: VecDeque::new(),
    }))
}

fn captured_stderr(stderr: &Arc<Mutex<StderrCapture>>) -> String {
    let (head, tail) = stderr
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .texts();
    format!("{head}{tail}")
}

fn ready_addrs(text: &str) -> Vec<String> {
    text.match_indices("ready addr=http://")
        .filter_map(|(index, _)| {
            let rest = &text[index + "ready addr=http://".len()..];
            rest.split(|ch: char| ch.is_whitespace() || ch == '/')
                .next()
                .map(str::to_string)
                .filter(|addr| !addr.is_empty())
        })
        .collect()
}

fn wait_child_ready(child: &mut Child, stderr: &Arc<Mutex<StderrCapture>>) -> Result<String> {
    wait_child_ready_result(child, stderr)
}

fn wait_child_ready_result(
    child: &mut Child,
    stderr: &Arc<Mutex<StderrCapture>>,
) -> Result<String> {
    let deadline = Instant::now() + READY_WAIT;
    let mut last_http = String::from("no ready line yet");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            bail!(
                "grokptah-service exited {status}; stderr={}",
                captured_stderr(stderr)
            );
        }
        let text = captured_stderr(stderr);
        if let Some(addr) = ready_addrs(&text).last().cloned() {
            match probe_ready_http(&addr) {
                Ok(()) => return Ok(addr),
                Err(error) => last_http = error,
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "grokptah-service never became HTTP-ready ({last_http}); stderr={}",
                text
            );
        }
        thread::sleep(POLL);
    }
}

fn probe_ready_http(addr: &str) -> Result<(), String> {
    let socket: SocketAddr = addr
        .parse()
        .map_err(|error| format!("parse listen {addr}: {error}"))?;
    let mut stream = TcpStream::connect_timeout(&socket, Duration::from_millis(200))
        .map_err(|error| format!("connect {addr}: {error}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_millis(400)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(400)));
    let request = format!("GET /ready HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("write {addr}: {error}"))?;
    let mut buf = String::new();
    let _ = stream.read_to_string(&mut buf);
    let status = buf.lines().next().unwrap_or("empty-response");
    if http_control_plane_is_serving(status) {
        Ok(())
    } else {
        Err(format!("GET /ready {addr} -> {status}"))
    }
}

fn http_control_plane_is_serving(status: &str) -> bool {
    status.starts_with("HTTP/1.1 200")
        || status.starts_with("HTTP/1.0 200")
        || status.starts_with("HTTP/1.1 503")
        || status.starts_with("HTTP/1.0 503")
}

pub fn fixture_zero_growth_window() -> Duration {
    let value: Value = serde_json::from_slice(FIXTURE_BYTES).expect("always-on fixture");
    let period = value["supervisorPeriodMs"]
        .as_u64()
        .expect("supervisorPeriodMs");
    let periods = value["zeroGrowthSupervisorPeriods"]
        .as_u64()
        .expect("zeroGrowthSupervisorPeriods");
    Duration::from_millis(period.saturating_mul(periods))
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

fn is_path_identity_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "workspace" | "sourceworkspace" | "cwd" | "displayname" | "title" | "promptpreview"
    ) || normalized.ends_with("id")
        || normalized.ends_with("ids")
        || normalized.ends_with("hash")
}

fn looks_like_protocol_opaque_token(token: &str) -> bool {
    if token.len() < 40
        || token.bytes().all(|byte| byte.is_ascii_hexdigit())
        || (token.len() == 71
            && token.starts_with("opaque-")
            && token[7..].bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return false;
    }
    let has_alpha = token.bytes().any(|byte| byte.is_ascii_alphabetic());
    let digit_count = token.bytes().filter(u8::is_ascii_digit).count();
    let distinct = token
        .bytes()
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    has_alpha && digit_count >= 6 && distinct >= 12
}

fn redact_high_entropy_tokens(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut current = String::new();
    let flush = |out: &mut String, current: &mut String| {
        if looks_like_protocol_opaque_token(current) {
            out.push_str("<opaque-id>");
        } else {
            out.push_str(current);
        }
        current.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            current.push(ch);
        } else {
            if !current.is_empty() {
                flush(&mut out, &mut current);
            }
            out.push(ch);
        }
    }
    if !current.is_empty() {
        flush(&mut out, &mut current);
    }
    out
}

fn redact_protocol_identity_value(value: &Value) -> Value {
    match value {
        Value::String(_) => Value::String("<opaque-id>".into()),
        Value::Array(items) => {
            Value::Array(items.iter().map(redact_protocol_identity_value).collect())
        }
        other => project_public_mcp_for_secret_scan(other),
    }
}

pub fn project_public_mcp_for_secret_scan(value: &Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text)
            .map(|parsed| project_public_mcp_for_secret_scan(&parsed))
            .unwrap_or_else(|_| Value::String(redact_high_entropy_tokens(text))),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, nested)| {
                    let projected = if is_path_identity_key(key) {
                        redact_protocol_identity_value(nested)
                    } else {
                        project_public_mcp_for_secret_scan(nested)
                    };
                    (key.clone(), projected)
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(project_public_mcp_for_secret_scan)
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn scan_mcp_value(label: &str, value: &Value) -> Result<()> {
    scan_cert_text(label, &value.to_string())?;
    scan_value_for_forbidden_data(&project_public_mcp_for_secret_scan(value))
        .map_err(|error| anyhow::anyhow!("{label} failed forbidden-data scan: {error}"))?;
    Ok(())
}

fn scan_home(path: &Path) -> Result<()> {
    let mut files = 0u64;
    scan_home_inner(path, 0, &mut files)
}

fn scan_home_inner(path: &Path, depth: u64, files: &mut u64) -> Result<()> {
    if depth > MAX_SCAN_DEPTH {
        bail!("home scan depth {depth} exceeds ceiling {MAX_SCAN_DEPTH}");
    }
    let entries =
        std::fs::read_dir(path).with_context(|| format!("read_dir {}", path.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("dirent {}", path.display()))?;
        let child = entry.path();
        let meta = std::fs::symlink_metadata(&child)
            .with_context(|| format!("metadata {}", child.display()))?;
        if meta.file_type().is_dir() {
            scan_home_inner(&child, depth + 1, files)?;
            continue;
        }
        *files = files.saturating_add(1);
        if *files > MAX_SCAN_FILES {
            bail!(
                "home scan file count {} exceeds ceiling {MAX_SCAN_FILES}",
                *files
            );
        }
        let bytes = std::fs::read(&child).with_context(|| format!("read {}", child.display()))?;
        if bytes.len() as u64 > MAX_SCAN_FILE_BYTES {
            bail!(
                "home scan {} is {} bytes, ceiling {MAX_SCAN_FILE_BYTES}",
                child.display(),
                bytes.len()
            );
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| {
            anyhow::anyhow!(
                "home scan {} is binary or non-UTF8 ({} bytes)",
                child.display(),
                bytes.len()
            )
        })?;
        scan_cert_text(&format!("home {}", child.display()), text)?;
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
        assert_eq!(value["schemaVersion"], 2);
        assert_eq!(
            value["proposalOnlyEnforcement"],
            "unverified-pending-pr-352"
        );
        assert_eq!(value["soak24h"], "unverified-no-pinned-head-artifact");
        assert_eq!(
            value["provedOracle"],
            "interrupted_run_not_readmitted_within_window"
        );
        assert_eq!(value["clock"], "bounded-race-controlled-no-fake-clock-seam");
        assert_eq!(
            value["failClosed"]["malformed"]["runState"],
            "limit_reached"
        );
        assert_eq!(
            value["failClosed"]["malformed"]["stopCause"],
            "token_accounting_unavailable"
        );
        assert_eq!(
            value["happyPath"]["providerPostsBySemanticId"]["step-b-fix"],
            1
        );
        let projected = project_public_mcp_for_secret_scan(&json!({
            "agentId": "agent-550e8400-e29b-41d4-a716-446655440000",
            "spec": {"displayName": "agent-550e8400-e29b-41d4-a716-446655440000"},
            "state": "interrupted"
        }));
        assert_eq!(projected["agentId"], "<opaque-id>");
        assert_eq!(projected["spec"]["displayName"], "<opaque-id>");
        scan_value_for_forbidden_data(&projected).unwrap();
    }
}
