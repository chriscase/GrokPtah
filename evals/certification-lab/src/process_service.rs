//! Process-level standalone `grokptah-service` plus a loopback fake provider.
//!
//! Used only by the always-on Grokbot certification probe. The parent process
//! may be running the in-process lab host; the child is isolated by
//! `GROKPTAH_HOME` and has `GROKPTAH_AGENT_OFFLINE` removed.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::McpControlClient;
use serde_json::{json, Value};
use tempfile::TempDir;

pub const TOKEN: &str = "always-on-grokbot-cert-token-32chars";
const SYNTHETIC_KEY: &str = "test-not-a-secret";
const READY_WAIT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(20);

pub struct FakeProvider {
    pub base_url: String,
    sends: Arc<AtomicU64>,
    _join: thread::JoinHandle<()>,
}

impl FakeProvider {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake provider");
        let addr = listener.local_addr().expect("local addr");
        let sends = Arc::new(AtomicU64::new(0));
        let sends_task = Arc::clone(&sends);
        let join = thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let Some((head, body)) = read_http_message(&mut stream) else {
                    continue;
                };
                if head.starts_with("GET ") {
                    let body = r#"{"object":"list","data":[{"id":"grok-build","object":"model"}]}"#;
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                    continue;
                }
                sends_task.fetch_add(1, Ordering::SeqCst);
                let response = scripted_completion(&body);
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            base_url: format!("http://{addr}/v1"),
            sends,
            _join: join,
        }
    }

    pub fn send_count(&self) -> u64 {
        self.sends.load(Ordering::SeqCst)
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
        if let Some(header_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
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
            return Some((
                first,
                String::from_utf8_lossy(&buf[body_start..]).into_owned(),
            ));
        }
        if buf.len() > 1024 * 1024 {
            return None;
        }
    }
    None
}

fn extract_user_content(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    let Some(messages) = value.get("messages").and_then(Value::as_array) else {
        return body.to_string();
    };
    for message in messages.iter().rev() {
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            return text.to_string();
        }
    }
    body.to_string()
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
            "assignedAgentId": agent_id
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
    let all = extract_all_text(body);
    if all.contains("Return exactly this JSON envelope") {
        return sse_ok(&rewrite_directive(&all));
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
        if let Some(text) = message.get("content").and_then(Value::as_str) {
            out.push_str(text);
            out.push('\n');
        }
    }
    if out.is_empty() {
        body.to_string()
    } else {
        out
    }
}

pub struct ProcessService {
    pub addr: String,
    pub workspace: PathBuf,
    child: Child,
    bin: PathBuf,
    provider: FakeProvider,
    home: PathBuf,
    _home: TempDir,
    _workspace: TempDir,
}

impl ProcessService {
    pub fn spawn() -> Result<Self> {
        let bin = std::env::var("GROKPTAH_SERVICE_BIN").context("GROKPTAH_SERVICE_BIN")?;
        if !Path::new(&bin).is_file() {
            bail!("GROKPTAH_SERVICE_BIN is not a file");
        }
        let provider = FakeProvider::start();
        let home = tempfile::tempdir().context("runtime home")?;
        let workspace_dir = tempfile::tempdir().context("workspace")?;
        let workspace = dunce::canonicalize(workspace_dir.path())?;
        let listen = {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let addr = listener.local_addr()?;
            drop(listener);
            addr.to_string()
        };
        let mut child = Command::new(&bin)
            .env("GROKPTAH_HOME", home.path())
            .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
            .env("GROKPTAH_SERVICE_LISTEN", &listen)
            .env("GROKPTAH_SERVICE_WORKSPACES", &workspace)
            .env("GROKPTAH_SERVICE_MAX_CONCURRENT", "4")
            .env("XAI_API_KEY", SYNTHETIC_KEY)
            .env("XAI_API_BASE", &provider.base_url)
            .env_remove("GROKPTAH_AGENT_OFFLINE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn grokptah-service")?;
        if let Some(mut stderr) = child.stderr.take() {
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while stderr.read(&mut buf).ok().is_some_and(|n| n > 0) {}
            });
        }
        wait_ready(&listen)?;
        Ok(Self {
            addr: listen,
            workspace,
            child,
            bin: PathBuf::from(bin),
            provider,
            home: dunce::canonicalize(home.path())?,
            _home: home,
            _workspace: workspace_dir,
        })
    }

    pub fn send_count(&self) -> u64 {
        self.provider.send_count()
    }

    pub fn respawn(&mut self) -> Result<()> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let listen = {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let addr = listener.local_addr()?;
            drop(listener);
            addr.to_string()
        };
        let mut child = Command::new(&self.bin)
            .env("GROKPTAH_HOME", &self.home)
            .env("GROKPTAH_SERVICE_TOKEN", TOKEN)
            .env("GROKPTAH_SERVICE_LISTEN", &listen)
            .env("GROKPTAH_SERVICE_WORKSPACES", &self.workspace)
            .env("GROKPTAH_SERVICE_MAX_CONCURRENT", "4")
            .env("XAI_API_KEY", SYNTHETIC_KEY)
            .env("XAI_API_BASE", &self.provider.base_url)
            .env_remove("GROKPTAH_AGENT_OFFLINE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("respawn grokptah-service")?;
        if let Some(mut stderr) = child.stderr.take() {
            thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while stderr.read(&mut buf).ok().is_some_and(|n| n > 0) {}
            });
        }
        wait_ready(&listen)?;
        self.child = child;
        self.addr = listen;
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
