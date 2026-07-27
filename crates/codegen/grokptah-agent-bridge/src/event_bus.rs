//! Fan-out event publisher + bounded redacted journal (#196).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::events::SessionUpdate;

/// Default max journal entries retained for cursor replay.
pub const DEFAULT_JOURNAL_CAPACITY: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntry {
    pub seq: u64,
    pub ts: String,
    /// Redacted session update (no secrets / large tool bodies).
    pub update: SessionUpdate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalPage {
    pub entries: Vec<JournalEntry>,
    pub next_cursor: Option<u64>,
    pub cursor_expired: bool,
}

struct BusInner {
    subscribers: Vec<mpsc::UnboundedSender<SessionUpdate>>,
    journal: VecDeque<JournalEntry>,
    capacity: usize,
    /// Next sequence number to allocate (starts at 1).
    next_seq: u64,
    /// Lowest seq still in the journal (for cursor expiry).
    oldest_seq: u64,
    journal_path: Option<PathBuf>,
    /// Optional configured control secrets to scrub from durable text.
    control_secrets: Vec<String>,
}

/// Multi-subscriber event bus with monotonic sequence numbers.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BusInner {
                subscribers: Vec::new(),
                journal: VecDeque::new(),
                capacity: capacity.max(1),
                next_seq: 1,
                oldest_seq: 1,
                journal_path: None,
                control_secrets: Vec::new(),
            })),
        }
    }

    /// Register secrets that must never appear in the durable journal.
    pub fn with_control_secrets(self, secrets: impl IntoIterator<Item = String>) -> Self {
        self.add_control_secrets(secrets);
        self
    }

    /// Register secrets on a shared bus handle (desktop/orchestration start).
    pub fn add_control_secrets(&self, secrets: impl IntoIterator<Item = String>) {
        let mut g = self.inner.lock();
        for s in secrets {
            if !s.is_empty() && !g.control_secrets.iter().any(|x| x == &s) {
                g.control_secrets.push(s);
            }
        }
    }

    /// Secrets currently configured for durable redaction (test/introspection).
    pub fn control_secrets_len(&self) -> usize {
        self.inner.lock().control_secrets.len()
    }

    /// Persist journal under `dir/event_journal.jsonl`; reload tail; compact file.
    pub fn with_persist_dir(self, dir: impl AsRef<Path>) -> Self {
        let path = dir.as_ref().join("event_journal.jsonl");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        {
            let mut g = self.inner.lock();
            if path.is_file() {
                if let Ok(loaded) = load_journal_file(&path, g.capacity) {
                    if let Some(last) = loaded.back() {
                        g.next_seq = last.seq.saturating_add(1);
                        g.oldest_seq = loaded.front().map(|e| e.seq).unwrap_or(1);
                    }
                    g.journal = loaded;
                    // Compact durable file to current in-memory tail.
                    let _ = rewrite_journal_file(&path, &g.journal);
                }
            }
            g.journal_path = Some(path);
        }
        self
    }

    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionUpdate> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.lock().subscribers.push(tx);
        rx
    }

    /// Publish: allocate seq + journal insert + fan-out under one lock (monotonic).
    pub fn publish(&self, update: SessionUpdate) {
        let mut g = self.inner.lock();
        let seq = g.next_seq;
        g.next_seq = g.next_seq.saturating_add(1);
        let ts = chrono::Utc::now().to_rfc3339();
        let secrets = g.control_secrets.clone();
        let redacted = redact_update_with_secrets(update.clone(), &secrets);
        let entry = JournalEntry {
            seq,
            ts,
            update: redacted,
        };
        g.journal.push_back(entry.clone());
        while g.journal.len() > g.capacity {
            if let Some(old) = g.journal.pop_front() {
                g.oldest_seq = old.seq.saturating_add(1);
            }
        }
        // Bound durable file: rewrite when over 2× capacity lines.
        if let Some(path) = g.journal_path.clone() {
            if let Ok(meta) = std::fs::metadata(&path) {
                // Rough: if file larger than capacity * 4KB, compact.
                if meta.len() > (g.capacity as u64).saturating_mul(4096) {
                    let _ = rewrite_journal_file(&path, &g.journal);
                } else {
                    append_journal_line(&path, &entry);
                }
            } else {
                append_journal_line(&path, &entry);
            }
        }
        // Drop closed subscribers so growth stays bounded.
        g.subscribers.retain(|tx| tx.send(update.clone()).is_ok());
    }

    pub fn send(&self, update: SessionUpdate) -> Result<(), mpsc::error::SendError<SessionUpdate>> {
        self.publish(update);
        Ok(())
    }

    /// Cursor is exclusive: return entries with `seq > after_seq`.
    pub fn read_after(&self, after_seq: u64, limit: usize) -> JournalPage {
        let limit = limit.clamp(1, 500);
        let g = self.inner.lock();
        if after_seq > 0 && after_seq + 1 < g.oldest_seq && !g.journal.is_empty() {
            return JournalPage {
                entries: Vec::new(),
                next_cursor: None,
                cursor_expired: true,
            };
        }
        let entries: Vec<JournalEntry> = g
            .journal
            .iter()
            .filter(|e| e.seq > after_seq)
            .take(limit)
            .cloned()
            .collect();
        let next_cursor = entries.last().map(|e| e.seq);
        let cursor_expired = entries.is_empty()
            && after_seq > 0
            && after_seq < g.oldest_seq
            && !g.journal.is_empty();
        JournalPage {
            entries: if cursor_expired { Vec::new() } else { entries },
            next_cursor,
            cursor_expired,
        }
    }

    /// Page through entire run range (honors cursor expiry; no silent 500 cutoff).
    pub fn read_range_all(
        &self,
        after_exclusive: u64,
        end_inclusive: Option<u64>,
        session_filter: Option<uuid::Uuid>,
    ) -> Result<Vec<JournalEntry>, CursorExpiredError> {
        let mut after = after_exclusive;
        let mut out = Vec::new();
        loop {
            let page = self.read_after(after, 500);
            if page.cursor_expired {
                return Err(CursorExpiredError);
            }
            if page.entries.is_empty() {
                break;
            }
            for e in page.entries {
                if let Some(end) = end_inclusive {
                    if e.seq > end {
                        return Ok(out);
                    }
                }
                if let Some(sid) = session_filter {
                    if session_id_of(&e.update) != Some(sid) {
                        after = e.seq;
                        continue;
                    }
                }
                after = e.seq;
                out.push(e);
            }
        }
        Ok(out)
    }

    pub fn current_seq(&self) -> u64 {
        let g = self.inner.lock();
        g.next_seq.saturating_sub(1)
    }

    pub fn oldest_seq(&self) -> u64 {
        self.inner.lock().oldest_seq
    }

    pub fn subscriber_count(&self) -> usize {
        self.inner.lock().subscribers.len()
    }
}

#[derive(Debug)]
pub struct CursorExpiredError;

fn session_id_of(u: &SessionUpdate) -> Option<uuid::Uuid> {
    use SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
        | TurnComplete { session_id, .. }
        | Error { session_id, .. }
        | SubagentSpawned { session_id, .. }
        | SubagentUpdate { session_id, .. }
        | ShellSessionStarted { session_id, .. }
        | ShellOutput { session_id, .. }
        | ShellSessionEnded { session_id, .. }
        | FileEdit { session_id, .. }
        | AgentProgress { session_id, .. }
        | RateLimited { session_id, .. }
        | SteeringInjected { session_id, .. } => Some(*session_id),
        BackgroundTask { session_id, .. } => *session_id,
    }
}

fn append_journal_line(path: &Path, entry: &JournalEntry) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        if let Ok(line) = serde_json::to_string(entry) {
            let _ = writeln!(f, "{line}");
        }
    }
}

fn rewrite_journal_file(path: &Path, journal: &VecDeque<JournalEntry>) -> std::io::Result<()> {
    let tmp = path.with_extension("jsonl.tmp");
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        for e in journal {
            writeln!(f, "{}", serde_json::to_string(e).unwrap_or_default())?;
        }
        f.sync_all()?;
    }
    std::fs::rename(tmp, path)?;
    Ok(())
}

fn load_journal_file(path: &Path, capacity: usize) -> std::io::Result<VecDeque<JournalEntry>> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path)?;
    let reader = BufReader::new(f);
    let mut all: VecDeque<JournalEntry> = VecDeque::new();
    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<JournalEntry>(line) {
            all.push_back(entry);
            while all.len() > capacity {
                all.pop_front();
            }
        }
    }
    Ok(all)
}

/// Public redaction entry used by tests and publish path.
pub fn redact_update(update: SessionUpdate) -> SessionUpdate {
    redact_update_with_secrets(update, &[])
}

pub fn redact_update_with_secrets(
    mut update: SessionUpdate,
    control_secrets: &[String],
) -> SessionUpdate {
    match &mut update {
        SessionUpdate::ToolCall { input, title, .. } => {
            *input = redact_json(input, control_secrets);
            if title.to_ascii_lowercase().contains("auth")
                || title.to_ascii_lowercase().contains("secret")
                || title.to_ascii_lowercase().contains("token")
            {
                *title = "redacted_tool".into();
            } else {
                *title = scrub_text(title, control_secrets, 500);
            }
        }
        SessionUpdate::ToolCallUpdate {
            output: Some(o), ..
        } => {
            *o = scrub_text(o, control_secrets, 2_000);
        }
        SessionUpdate::ToolCallUpdate { output: None, .. } => {}
        SessionUpdate::AgentMessageChunk { text, .. }
        | SessionUpdate::AgentThoughtChunk { text, .. }
        | SessionUpdate::Error { message: text, .. }
        | SessionUpdate::RateLimited { message: text, .. } => {
            *text = scrub_text(text, control_secrets, 4_000);
        }
        SessionUpdate::FileEdit {
            unified_diff,
            summary,
            path,
            ..
        } => {
            *unified_diff = scrub_text(unified_diff, control_secrets, 4_000);
            *summary = scrub_text(summary, control_secrets, 500);
            *path = scrub_text(path, control_secrets, 500);
        }
        SessionUpdate::ShellOutput { data, .. } => {
            *data = scrub_text(data, control_secrets, 4_000);
        }
        SessionUpdate::ShellSessionStarted { command, .. } => {
            *command = scrub_text(command, control_secrets, 2_000);
        }
        SessionUpdate::SteeringInjected { text, .. } => {
            *text = scrub_text(text, control_secrets, 2_000);
        }
        SessionUpdate::PermissionRequired { request, .. } => {
            request.detail = redact_json(&request.detail, control_secrets);
            request.summary = scrub_text(&request.summary, control_secrets, 500);
        }
        SessionUpdate::SubagentUpdate { detail, .. } => {
            if let Some(d) = detail {
                *d = scrub_text(d, control_secrets, 2_000);
            }
        }
        SessionUpdate::BackgroundTask { title, .. } => {
            *title = scrub_text(title, control_secrets, 500);
        }
        SessionUpdate::AgentProgress {
            detail, last_tool, ..
        } => {
            *detail = scrub_text(detail, control_secrets, 500);
            if let Some(t) = last_tool {
                *t = scrub_text(t, control_secrets, 200);
            }
        }
        SessionUpdate::Plan { steps, .. } => {
            for s in steps.iter_mut() {
                *s = scrub_text(s, control_secrets, 500);
            }
        }
        SessionUpdate::TurnComplete { .. }
        | SessionUpdate::ShellSessionEnded { .. }
        | SessionUpdate::SubagentSpawned { .. } => {}
    }
    update
}

fn redact_json(v: &serde_json::Value, secrets: &[String]) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let lk = k.to_ascii_lowercase();
                if lk.contains("token")
                    || lk.contains("secret")
                    || lk.contains("password")
                    || lk.contains("api_key")
                    || lk.contains("authorization")
                    || lk.contains("bearer")
                {
                    out.insert(k.clone(), serde_json::json!("[redacted]"));
                } else {
                    out.insert(k.clone(), redact_json(val, secrets));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(|x| redact_json(x, secrets)).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(scrub_text(s, secrets, 2_000)),
        other => other.clone(),
    }
}

/// Remove bearer values and secrets entirely (no "marker + original").
fn scrub_text(s: &str, secrets: &[String], max: usize) -> String {
    let mut out = s.to_string();
    // Bearer TOKEN → Bearer [redacted] (token removed)
    if let Some(idx) = out.find("Bearer ") {
        let rest = &out[idx + "Bearer ".len()..];
        let token_end = rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',')
            .unwrap_or(rest.len());
        let before = &out[..idx];
        let after = &rest[token_end..];
        out = format!("{before}Bearer [redacted]{after}");
    }
    // Authorization: Bearer ...
    out = regex_lite_replace_auth(&out);
    // Common credential assignments
    out = scrub_assignment(&out, "GROKPTAH_CONTROL_TOKEN");
    out = scrub_assignment(&out, "API_KEY");
    out = scrub_assignment(&out, "OPENAI_API_KEY");
    out = scrub_assignment(&out, "XAI_API_KEY");
    for secret in secrets {
        if !secret.is_empty() && out.contains(secret) {
            out = out.replace(secret, "[redacted]");
        }
    }
    if out.len() <= max {
        out
    } else {
        let head = crate::textutil::truncate_at_char_boundary(&out, max);
        format!("{head}…[truncated {} bytes]", out.len())
    }
}

fn regex_lite_replace_auth(s: &str) -> String {
    // Strip "authorization: <anything until whitespace>"
    let lower = s.to_ascii_lowercase();
    if let Some(idx) = lower.find("authorization:") {
        let after = idx + "authorization:".len();
        let rest = &s[after..];
        let end = rest.find(['\n', '\r']).unwrap_or(rest.len());
        format!("{}authorization: [redacted]{}", &s[..idx], &rest[end..])
    } else {
        s.to_string()
    }
}

fn scrub_assignment(s: &str, key: &str) -> String {
    // KEY=value or KEY=value; KEY: value
    let patterns = [format!("{key}="), format!("{key}:"), format!("{key} =")];
    let mut out = s.to_string();
    for p in patterns {
        if let Some(idx) = out.find(&p) {
            let start = idx + p.len();
            let rest = &out[start..];
            let end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ';')
                .unwrap_or(rest.len());
            out = format!("{}{}[redacted]{}", &out[..start], "", &rest[end..]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn fan_out_preserves_order_for_two_subscribers() {
        let bus = EventBus::new(64);
        let mut a = bus.subscribe();
        let mut b = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..5 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("m{i}"),
            });
        }
        for i in 0..5 {
            let ua = a.try_recv().unwrap();
            let ub = b.try_recv().unwrap();
            match (ua, ub) {
                (
                    SessionUpdate::AgentMessageChunk { text: ta, .. },
                    SessionUpdate::AgentMessageChunk { text: tb, .. },
                ) => {
                    assert_eq!(ta, format!("m{i}"));
                    assert_eq!(tb, format!("m{i}"));
                }
                _ => panic!("wrong variant"),
            }
        }
    }

    #[test]
    fn journal_cursor_and_expiry() {
        let bus = EventBus::new(3);
        let sid = Uuid::new_v4();
        for i in 0..5 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("{i}"),
            });
        }
        let page = bus.read_after(0, 10);
        assert!(!page.cursor_expired);
        assert_eq!(page.entries.len(), 3);
        let expired = bus.read_after(1, 10);
        assert!(expired.cursor_expired);
    }

    #[test]
    fn redact_removes_bearer_token_entirely() {
        let sid = Uuid::new_v4();
        let u = SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "use Bearer super-secret-token-xyz for auth".into(),
        };
        let r = redact_update(u);
        if let SessionUpdate::AgentMessageChunk { text, .. } = r {
            assert!(text.contains("Bearer [redacted]"));
            assert!(!text.contains("super-secret-token-xyz"));
        } else {
            panic!("variant");
        }
    }

    #[test]
    fn redact_strips_token_fields() {
        let sid = Uuid::new_v4();
        let u = SessionUpdate::ToolCall {
            session_id: sid,
            call_id: "c1".into(),
            title: "x".into(),
            kind: crate::events::ToolCallKind::Other,
            status: crate::events::ToolCallStatus::Running,
            input: serde_json::json!({"api_key": "sk-secret", "path": "a.rs"}),
        };
        let r = redact_update(u);
        if let SessionUpdate::ToolCall { input, .. } = r {
            assert_eq!(input["api_key"], "[redacted]");
            assert_eq!(input["path"], "a.rs");
        } else {
            panic!("variant");
        }
    }

    #[test]
    fn control_secret_scrubbed_from_shell_command() {
        let sid = Uuid::new_v4();
        let secret = "ctl-token-abc123".to_string();
        let u = SessionUpdate::ShellSessionStarted {
            session_id: sid,
            call_id: "c".into(),
            command: format!("export GROKPTAH_CONTROL_TOKEN={secret} && env"),
        };
        let r = redact_update_with_secrets(u, std::slice::from_ref(&secret));
        if let SessionUpdate::ShellSessionStarted { command, .. } = r {
            assert!(!command.contains(&secret));
            assert!(command.contains("[redacted]"));
        } else {
            panic!("variant");
        }
    }

    #[test]
    fn journal_reloads_from_disk_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let sid = Uuid::new_v4();
        let bus1 = EventBus::new(64).with_persist_dir(dir.path());
        for i in 0..3 {
            bus1.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("m{i}"),
            });
        }
        let seq_before = bus1.current_seq();
        drop(bus1);

        let bus2 = EventBus::new(64).with_persist_dir(dir.path());
        let page = bus2.read_after(0, 100);
        assert!(!page.cursor_expired);
        assert_eq!(page.entries.len(), 3);
        assert_eq!(bus2.current_seq(), seq_before);
        bus2.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "m3".into(),
        });
        assert_eq!(bus2.current_seq(), seq_before + 1);
    }

    #[test]
    fn concurrent_publish_is_strictly_monotonic() {
        use std::sync::Arc;
        use std::thread;
        let bus = Arc::new(EventBus::new(10_000));
        let sid = Uuid::new_v4();
        let mut handles = Vec::new();
        for t in 0..8 {
            let bus = bus.clone();
            handles.push(thread::spawn(move || {
                for i in 0..50 {
                    bus.publish(SessionUpdate::AgentMessageChunk {
                        session_id: sid,
                        text: format!("{t}-{i}"),
                    });
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let page = bus.read_after(0, 500);
        assert_eq!(page.entries.len(), 400);
        let mut last = 0u64;
        for e in &page.entries {
            assert!(e.seq > last, "seq not monotonic: {} then {}", last, e.seq);
            last = e.seq;
        }
        // no duplicates
        let mut seen = std::collections::HashSet::new();
        for e in &page.entries {
            assert!(seen.insert(e.seq));
        }
    }

    #[test]
    fn secret_absent_after_persist_reload() {
        let dir = tempfile::tempdir().unwrap();
        let secret = "persist-secret-ZZZ".to_string();
        let sid = Uuid::new_v4();
        let bus1 = EventBus::new(64)
            .with_control_secrets([secret.clone()])
            .with_persist_dir(dir.path());
        bus1.publish(SessionUpdate::Error {
            session_id: sid,
            message: format!("failed with {secret}"),
        });
        drop(bus1);
        let disk = std::fs::read_to_string(dir.path().join("event_journal.jsonl")).unwrap();
        assert!(!disk.contains(&secret), "secret leaked to disk: {disk}");
        let bus2 = EventBus::new(64)
            .with_control_secrets([secret.clone()])
            .with_persist_dir(dir.path());
        let page = bus2.read_after(0, 10);
        let text = serde_json::to_string(&page.entries).unwrap();
        assert!(!text.contains(&secret));
    }
}
