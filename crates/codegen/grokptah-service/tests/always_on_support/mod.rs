//! Process-level harness for the always-on Grokbot campaign.
//!
//! Spawns the shipped `grokptah-service` binary, a loopback fake provider with
//! an explicit POST barrier, and an authenticated MCP client. No production
//! crate is modified.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use grokptah_agent_bridge::orchestration::hash_payload;
use grokptah_agent_bridge::{scan_value_for_forbidden_data, McpControlClient};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
pub const SYNTHETIC_KEY: &str = "test-not-a-secret";
pub const FIXTURE_BYTES: &[u8] = include_bytes!("../fixtures/always_on_grokbot.json");
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

#[derive(Clone, Debug)]
pub struct Fixture {
    pub schema: String,
    pub schema_version: u64,
    pub seed: String,
    pub base_sha: String,
    pub success: String,
    pub fail: String,
    pub ok: String,
    pub setup: String,
    pub step_first: String,
    pub step_failing: String,
    pub step_replacement: String,
    pub proposal_only: String,
    pub internal_persistence_cuts: String,
    pub attempt_evidence: String,
    pub clock: String,
    pub soak24h: String,
    pub required_assertions: Vec<String>,
    pub posts_by_semantic: BTreeMap<String, u64>,
}

impl Fixture {
    pub fn load() -> Self {
        let value: Value = serde_json::from_slice(FIXTURE_BYTES).expect("fixture JSON");
        let schema = value["schema"].as_str().expect("schema").to_string();
        assert_eq!(schema, FIXTURE_SCHEMA, "always-on fixture schema mismatch");
        let schema_version = value["schemaVersion"].as_u64().expect("schemaVersion");
        assert_eq!(
            schema_version, 1,
            "always-on fixture schemaVersion mismatch"
        );
        let posts = value["happyPath"]["providerPostsBySemanticId"]
            .as_object()
            .expect("providerPostsBySemanticId")
            .iter()
            .map(|(key, item)| (key.clone(), item.as_u64().expect("post count")))
            .collect();
        let required = value["requiredAssertions"]
            .as_array()
            .expect("requiredAssertions")
            .iter()
            .map(|item| item.as_str().expect("assertion id").to_string())
            .collect();
        Self {
            schema,
            schema_version,
            seed: value["seed"].as_str().expect("seed").to_string(),
            base_sha: value["baseSha"].as_str().expect("baseSha").to_string(),
            success: value["sentinels"]["success"]
                .as_str()
                .expect("success")
                .to_string(),
            fail: value["sentinels"]["fail"]
                .as_str()
                .expect("fail")
                .to_string(),
            ok: value["sentinels"]["ok"].as_str().expect("ok").to_string(),
            setup: value["sentinels"]["setup"]
                .as_str()
                .expect("setup")
                .to_string(),
            step_first: value["steps"]["first"].as_str().expect("first").to_string(),
            step_failing: value["steps"]["failing"]
                .as_str()
                .expect("failing")
                .to_string(),
            step_replacement: value["steps"]["replacement"]
                .as_str()
                .expect("replacement")
                .to_string(),
            proposal_only: value["proposalOnlyEnforcement"]
                .as_str()
                .expect("proposalOnlyEnforcement")
                .to_string(),
            internal_persistence_cuts: value["internalPersistenceCuts"]
                .as_str()
                .expect("internalPersistenceCuts")
                .to_string(),
            attempt_evidence: value["attemptEvidence"]
                .as_str()
                .expect("attemptEvidence")
                .to_string(),
            clock: value["clock"].as_str().expect("clock").to_string(),
            soak24h: value["soak24h"].as_str().expect("soak24h").to_string(),
            required_assertions: required,
            posts_by_semantic: posts,
        }
    }

    pub fn digest(&self) -> String {
        hash_payload(&serde_json::from_slice(FIXTURE_BYTES).expect("fixture value"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderScript {
    Lifecycle,
    InvalidDirective,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderDisposition {
    Scripted,
    Hold,
    Drop,
    Status500,
    Malformed,
    Slow,
}

#[derive(Clone, Debug, Eq, PartialEq)]
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
    script: Mutex<ProviderScript>,
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
        Self::start_with(ProviderScript::Lifecycle)
    }

    pub fn start_with(script: ProviderScript) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let addr = listener.local_addr().expect("local addr");
        let state = Arc::new(ProviderState {
            script: Mutex::new(script),
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
            listener.set_nonblocking(false).ok();
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
        if disposition != ProviderDisposition::Hold {
            self.release(semantic_id);
        }
    }

    pub fn release(&self, semantic_id: &str) {
        self.state
            .release_set
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(semantic_id.to_string());
        self.state.release_signal.notify_all();
    }

    pub fn wait_accepted(&self, semantic_id: &str, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut accepted = self
            .state
            .accepted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if accepted.get(semantic_id).copied().unwrap_or(0) >= 1 {
                return;
            }
            let now = Instant::now();
            assert!(
                now < deadline,
                "provider never accepted POST {semantic_id}; records={:?}",
                self.records()
            );
            let (guard, result) = self
                .state
                .accepted_signal
                .wait_timeout(accepted, deadline.saturating_duration_since(now))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            accepted = guard;
            if result.timed_out() {
                assert!(
                    accepted.get(semantic_id).copied().unwrap_or(0) >= 1,
                    "provider never accepted POST {semantic_id}; records={:?}",
                    self.records()
                );
                return;
            }
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

    pub fn assert_route_and_auth(&self) {
        for record in self.records() {
            assert!(
                record.route_ok,
                "unexpected provider route {} {}",
                record.method, record.path
            );
            assert!(
                !record.body_digest.contains(SYNTHETIC_KEY)
                    && !record.body_digest.contains(TOKEN)
                    && !record.semantic_id.contains(SYNTHETIC_KEY),
                "provider log stored a raw secret"
            );
            if record.method != "POST" {
                continue;
            }
            assert!(
                record.auth_present,
                "provider POST {} lacked Authorization presence",
                record.semantic_id
            );
            assert_eq!(record.auth_scheme.as_deref(), Some("bearer"));
        }
    }
}

fn handle_provider_conn(mut stream: TcpStream, state: &ProviderState) {
    let Some((head, headers, body)) = read_http_message(&mut stream) else {
        return;
    };
    let (method, path) = split_request_line(&head);
    if method == "GET" {
        let _ = stream.write_all(models_list().as_bytes());
        let _ = stream.flush();
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
    let script = *state
        .script
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match disposition {
        ProviderDisposition::Drop => {
            let _ = stream.shutdown(Shutdown::Both);
        }
        ProviderDisposition::Status500 => {
            let _ = stream.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 16\r\nConnection: close\r\n\r\nprovider-fail-v1",
            );
        }
        ProviderDisposition::Malformed => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{not-json",
            );
        }
        ProviderDisposition::Slow => {
            thread::sleep(Duration::from_secs(8));
            let response = scripted_completion(&body, script);
            let _ = stream.write_all(response.as_bytes());
        }
        ProviderDisposition::Scripted | ProviderDisposition::Hold => {
            let response = scripted_completion(&body, script);
            let _ = stream.write_all(response.as_bytes());
        }
    }
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
    let all = extract_all_text(body);
    if all.contains("Return exactly this JSON envelope") {
        return "manager-decision".into();
    }
    let content = extract_user_content(body);
    if content.contains("CERT_MALFORMED") {
        return "fail-malformed".into();
    }
    if content.contains("CERT_500") {
        return "fail-500".into();
    }
    if content.contains("CERT_DROP") {
        return "fail-drop".into();
    }
    if content.contains("CERT_SLOW") {
        return "fail-slow".into();
    }
    if content.contains("CERT_CANCEL") {
        return "fail-cancel".into();
    }
    if content.contains("GROKBOT_SETUP") {
        return "setup".into();
    }
    if content.contains("GROKBOT_FORCE_FAIL") {
        return "step-b".into();
    }
    if content.contains("GROKBOT_SUCCESS complete the replacement") {
        return "step-b-fix".into();
    }
    if content.contains("GROKBOT_SUCCESS first native unit") {
        return "step-a".into();
    }
    if content.contains("GROKBOT_SUCCESS") {
        return "native-success".into();
    }
    "other".into()
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
        if let Some(header_end) = find_double_crlf(&buf) {
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
            let body = String::from_utf8_lossy(&buf[body_start..]).into_owned();
            return Some((first, headers, body));
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

#[derive(Clone, Debug, Default)]
pub struct ResourceSample {
    pub rss_bytes: u64,
    pub fd_count: u64,
    pub threads: u64,
    pub disk_bytes: u64,
}

impl ResourceSample {
    pub fn max_with(&mut self, other: &Self) {
        self.rss_bytes = self.rss_bytes.max(other.rss_bytes);
        self.fd_count = self.fd_count.max(other.fd_count);
        self.threads = self.threads.max(other.threads);
        self.disk_bytes = self.disk_bytes.max(other.disk_bytes);
    }
}

pub struct ServiceProcess {
    pub addr: String,
    child: Child,
    pub home: PathBuf,
    pub workspace: PathBuf,
    stderr: Arc<Mutex<Vec<u8>>>,
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
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let child = spawn_service(provider_base, &home, &workspace, &listen, &stderr);
        wait_http_ready(&listen);
        Self {
            addr: listen,
            child,
            home,
            workspace,
            stderr,
            _home_dir: home_dir,
            _workspace_dir: workspace_dir,
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn sample(&self) -> ResourceSample {
        sample_pid(self.pid(), &self.home)
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

    pub fn respawn(&mut self, provider_base: &str) {
        self.kill_sigkill();
        let listen = free_listen();
        self.child = spawn_service(
            provider_base,
            &self.home,
            &self.workspace,
            &listen,
            &self.stderr,
        );
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

fn spawn_service(
    provider_base: &str,
    home: &Path,
    workspace: &Path,
    listen: &str,
    stderr_buf: &Arc<Mutex<Vec<u8>>>,
) -> Child {
    let bin = env!("CARGO_BIN_EXE_grokptah-service");
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
        .expect("spawn grokptah-service");
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

fn sample_pid(pid: u32, home: &Path) -> ResourceSample {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let mut rss_kb = 0u64;
    let mut threads = 0u64;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kb = value
                .split_whitespace()
                .next()
                .and_then(|item| item.parse().ok())
                .unwrap_or(0);
        }
        if let Some(value) = line.strip_prefix("Threads:") {
            threads = value.trim().parse().unwrap_or(0);
        }
    }
    let fd_count = std::fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|entries| entries.count() as u64)
        .unwrap_or(0);
    ResourceSample {
        rss_bytes: rss_kb.saturating_mul(1024),
        fd_count,
        threads,
        disk_bytes: dir_size(home),
    }
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        if meta.as_ref().is_some_and(|item| item.is_dir()) {
            total = total.saturating_add(dir_size(&entry.path()));
        } else if let Some(meta) = meta {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

pub async fn mcp(addr: &str) -> McpControlClient {
    mcp_with_token(addr, TOKEN).await
}

pub async fn mcp_with_token(addr: &str, token: &str) -> McpControlClient {
    let mut client = McpControlClient::new(format!("http://{addr}"), token);
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

pub async fn try_mcp(addr: &str, token: &str) -> Result<McpControlClient, String> {
    let mut client = McpControlClient::new(format!("http://{addr}"), token);
    client
        .initialize()
        .await
        .map(|_| client)
        .map_err(|error| error.to_string())
}

pub async fn call(client: &mut McpControlClient, tool: &str, args: Value) -> Value {
    let result = client
        .call_tool(tool, args)
        .await
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    scan_mcp(tool, &result.structured, &result.raw);
    assert!(!result.is_error, "{tool} error: {:?}", result.raw);
    result.structured
}

pub async fn call_expect_error(client: &mut McpControlClient, tool: &str, args: Value) -> String {
    match client.call_tool(tool, args).await {
        Ok(result) => {
            scan_mcp(tool, &result.structured, &result.raw);
            assert!(result.is_error, "{tool} should fail: {:?}", result.raw);
            result.raw.to_string()
        }
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

pub fn work_items(work: &Value) -> &[Value] {
    work.get("work")
        .or_else(|| work.get("items"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
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

pub fn work_for_step<'a>(work: &'a Value, step_id: &str) -> Vec<&'a Value> {
    work_items(work)
        .iter()
        .filter(|item| item["sourceManagerStepId"].as_str() == Some(step_id))
        .collect()
}

pub fn runs_array(runs: &Value) -> &[Value] {
    runs.get("runs")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn intents_array(intents: &Value) -> &[Value] {
    intents
        .get("intents")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn pending_usage(run: &Value) -> u64 {
    run.pointer("/aggregates/usagePendingRequests")
        .or_else(|| run.pointer("/aggregates/usage_pending_requests"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

pub fn assert_no_quota_ledger(value: &Value) {
    let encoded = value.to_string();
    assert!(
        !encoded.contains("QuotaLedger") && !encoded.contains("quotaLedger"),
        "quota ledger must be absent at 67e29bd: {value}"
    );
}

pub fn scan_text(label: &str, text: &str) {
    for sentinel in [
        TOKEN,
        SYNTHETIC_KEY,
        "XAI_API_KEY=",
        "GROKPTAH_SERVICE_TOKEN=",
        "GROKPTAH_TOKEN_COMMAND=",
    ] {
        assert!(
            !text.contains(sentinel),
            "{label} leaked sentinel {sentinel}"
        );
    }
    for sentinel in LIVE_URL_SENTINELS {
        assert!(!text.contains(sentinel), "{label} leaked live URL sentinel");
    }
}

pub fn scan_mcp(tool: &str, structured: &Value, raw: &Value) {
    scan_text(&format!("{tool} structured"), &structured.to_string());
    scan_text(&format!("{tool} raw"), &raw.to_string());
    let _ = scan_value_for_forbidden_data(structured);
    let _ = scan_value_for_forbidden_data(raw);
}

pub fn scan_home(home: &Path) {
    scan_home_inner(home, 0);
}

fn scan_home_inner(path: &Path, depth: usize) {
    if depth > 8 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        let meta = entry.metadata().ok();
        if meta.as_ref().is_some_and(|item| item.is_dir()) {
            scan_home_inner(&entry.path(), depth + 1);
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if bytes.len() > 1024 * 1024 {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&bytes) {
            scan_text(&format!("home {}", entry.path().display()), text);
        }
    }
}

pub fn scan_service_artifacts(service: &ServiceProcess) {
    scan_text("stderr", &service.stderr_text());
    scan_home(&service.home);
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetSnapshot {
    pub work_id: Option<String>,
    pub intent_id: Option<String>,
    pub attempt_id: Option<String>,
    pub run_id: Option<String>,
    pub provider_posts: u64,
    pub work_state: Option<String>,
    pub run_state: Option<String>,
    pub intent_state: Option<String>,
}

pub fn snapshot_step(
    work: &Value,
    intents: &Value,
    runs: &Value,
    step_id: &str,
    posts: u64,
) -> TargetSnapshot {
    let items = work_for_step(work, step_id);
    let work_item = items.first().copied();
    let work_id = work_item.and_then(|item| item["workId"].as_str().map(str::to_string));
    let intent = intents_array(intents).iter().find(|intent| {
        work_id
            .as_deref()
            .is_some_and(|id| intent["workId"].as_str() == Some(id))
    });
    let run_id = intent
        .and_then(|item| item.get("runId").or_else(|| item.get("run_id")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let attempt_id = intent
        .and_then(|item| item.get("attemptId").or_else(|| item.get("attempt_id")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let run = run_id.as_ref().and_then(|id| {
        runs_array(runs)
            .iter()
            .find(|run| run["runId"].as_str() == Some(id.as_str()))
    });
    TargetSnapshot {
        work_id,
        intent_id: intent.and_then(|item| item["intentId"].as_str().map(str::to_string)),
        attempt_id,
        run_id,
        provider_posts: posts,
        work_state: work_item.and_then(|item| item["state"].as_str().map(str::to_string)),
        run_state: run.and_then(|item| item["state"].as_str().map(str::to_string)),
        intent_state: intent.and_then(|item| item["state"].as_str().map(str::to_string)),
    }
}

pub fn assert_no_duplicate_step(work: &Value, step_id: &str) {
    assert_eq!(
        work_for_step(work, step_id).len(),
        1,
        "step {step_id} must have exactly one Work: {work}"
    );
}

pub fn repository_commit() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

static SERIAL: Mutex<()> = Mutex::new(());
static RECORDED: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

pub fn serial_lock() -> std::sync::MutexGuard<'static, ()> {
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn clear_assertions() {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

pub fn record_assertion(name: &str) {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(name.to_string());
}

pub fn recorded_assertions() -> BTreeSet<String> {
    RECORDED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}
