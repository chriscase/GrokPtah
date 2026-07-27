//! Fan-out event publisher + bounded redacted journal (#196).
//!
//! Replaces single-consumer `mpsc` so the desktop GUI and MCP control
//! subscribers receive the same ordered session stream without consuming
//! each other.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Lowest seq still in the journal (for cursor expiry).
    oldest_seq: u64,
    journal_path: Option<PathBuf>,
}

/// Multi-subscriber event bus with monotonic sequence numbers.
#[derive(Clone)]
pub struct EventBus {
    seq: Arc<AtomicU64>,
    inner: Arc<Mutex<BusInner>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            seq: Arc::new(AtomicU64::new(1)),
            inner: Arc::new(Mutex::new(BusInner {
                subscribers: Vec::new(),
                journal: VecDeque::new(),
                capacity: capacity.max(1),
                oldest_seq: 1,
                journal_path: None,
            })),
        }
    }

    /// Persist journal snapshots under `dir/event_journal.jsonl` (best-effort).
    /// Reloads existing journal lines so run-scoped reads survive process restart.
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
                        // Next publish seq must be last.seq + 1
                        self.seq.store(last.seq.saturating_add(1), Ordering::SeqCst);
                        g.oldest_seq = loaded.front().map(|e| e.seq).unwrap_or(1);
                    }
                    g.journal = loaded;
                }
            }
            g.journal_path = Some(path);
        }
        self
    }

    /// Subscribe for live fan-out of raw (GUI-compatible) session updates.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<SessionUpdate> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.inner.lock().subscribers.push(tx);
        rx
    }

    /// Publish an update to all live subscribers and the redacted journal.
    pub fn publish(&self, update: SessionUpdate) {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst);
        let ts = chrono::Utc::now().to_rfc3339();
        let redacted = redact_update(update.clone());
        let entry = JournalEntry {
            seq,
            ts,
            update: redacted,
        };

        let mut g = self.inner.lock();
        g.journal.push_back(entry.clone());
        while g.journal.len() > g.capacity {
            if let Some(old) = g.journal.pop_front() {
                g.oldest_seq = old.seq.saturating_add(1);
            }
        }
        if let Some(path) = g.journal_path.clone() {
            append_journal_line(&path, &entry);
        }
        g.subscribers.retain(|tx| tx.send(update.clone()).is_ok());
    }

    /// Compatibility with existing `let _ = event_tx.send(...)` call sites.
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
        let mut entries: Vec<JournalEntry> = g
            .journal
            .iter()
            .filter(|e| e.seq > after_seq)
            .take(limit)
            .cloned()
            .collect();
        let next_cursor = entries.last().map(|e| e.seq);
        // If nothing left after filter and after_seq is before oldest, expired.
        let cursor_expired = entries.is_empty()
            && after_seq > 0
            && after_seq < g.oldest_seq
            && !g.journal.is_empty();
        if cursor_expired {
            entries.clear();
        }
        JournalPage {
            entries,
            next_cursor,
            cursor_expired,
        }
    }

    pub fn current_seq(&self) -> u64 {
        self.seq.load(Ordering::SeqCst).saturating_sub(1)
    }

    pub fn oldest_seq(&self) -> u64 {
        self.inner.lock().oldest_seq
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

/// Load the tail of a durable journal, keeping at most `capacity` entries.
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

/// Redact secrets and truncate large tool bodies for the durable journal.
pub fn redact_update(mut update: SessionUpdate) -> SessionUpdate {
    match &mut update {
        SessionUpdate::ToolCall { input, title, .. } => {
            *input = redact_json(input);
            if title.to_ascii_lowercase().contains("auth")
                || title.to_ascii_lowercase().contains("secret")
            {
                *title = "redacted_tool".into();
            }
        }
        SessionUpdate::ToolCallUpdate {
            output: Some(o), ..
        } => {
            *o = truncate_redact(o, 2_000);
        }
        SessionUpdate::ToolCallUpdate { output: None, .. } => {}
        SessionUpdate::AgentMessageChunk { text, .. }
        | SessionUpdate::AgentThoughtChunk { text, .. }
        | SessionUpdate::Error { message: text, .. } => {
            *text = truncate_redact(text, 4_000);
        }
        SessionUpdate::FileEdit { unified_diff, .. }
        | SessionUpdate::ShellOutput {
            data: unified_diff, ..
        } => {
            *unified_diff = truncate_redact(unified_diff, 4_000);
        }
        SessionUpdate::SteeringInjected { text, .. } => {
            *text = truncate_redact(text, 2_000);
        }
        _ => {}
    }
    update
}

fn redact_json(v: &serde_json::Value) -> serde_json::Value {
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
                {
                    out.insert(k.clone(), serde_json::json!("[redacted]"));
                } else {
                    out.insert(k.clone(), redact_json(val));
                }
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(redact_json).collect())
        }
        serde_json::Value::String(s) => serde_json::Value::String(truncate_redact(s, 2_000)),
        other => other.clone(),
    }
}

fn truncate_redact(s: &str, max: usize) -> String {
    let s = s.replace("Bearer ", "Bearer [redacted] ");
    if s.len() <= max {
        s
    } else {
        // char-boundary safe (CJK / emoji) — never slice mid-codepoint
        let head = crate::textutil::truncate_at_char_boundary(&s, max);
        format!("{head}…[truncated {} bytes]", s.len())
    }
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
        // capacity 3 → only last 3 kept; cursor before oldest expires
        let page = bus.read_after(0, 10);
        assert!(!page.cursor_expired);
        assert_eq!(page.entries.len(), 3);
        let expired = bus.read_after(1, 10);
        assert!(expired.cursor_expired);
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
        // New publish continues sequence
        bus2.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "m3".into(),
        });
        assert_eq!(bus2.current_seq(), seq_before + 1);
        let page2 = bus2.read_after(seq_before, 10);
        assert_eq!(page2.entries.len(), 1);
        assert_eq!(page2.entries[0].seq, seq_before + 1);
    }
}
