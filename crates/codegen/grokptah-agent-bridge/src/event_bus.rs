//! Fan-out event publisher + bounded redacted journal (#196).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::events::SessionUpdate;

/// Default max journal entries retained for cursor replay.
pub const DEFAULT_JOURNAL_CAPACITY: usize = 4096;
const MAX_JOURNAL_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 256 * 1024;
const MAX_JOURNAL_LINE_BYTES: usize = MAX_EVENT_BYTES + 4096;
const SEQUENCE_RESERVATION_SIZE: u64 = 1_000_000;
/// Bounded wait when the durable writer is behind. Queue-full is backpressure,
/// not a missing durable range.
const WRITER_SEND_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// Writer has not acked the requested range. An empty page with this set
    /// is not a durable catch-up; callers must retry or fail closed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub writer_pending: bool,
}

struct BusInner {
    stream_tx: broadcast::Sender<SequencedUpdate>,
    critical_tx: broadcast::Sender<SequencedUpdate>,
    journal: VecDeque<JournalEntry>,
    journal_bytes: usize,
    capacity: usize,
    /// Next sequence number to allocate (starts at 1).
    next_seq: u64,
    /// Last sequence actually published (may trail a reserved range start).
    last_seq: u64,
    reserved_through: u64,
    sequence_path: Option<PathBuf>,
    /// Lowest seq still in the journal (for cursor expiry).
    oldest_seq: u64,
    persistence: Option<Arc<PersistenceHandle>>,
    /// Optional configured control secrets to scrub from durable text.
    control_secrets: Vec<String>,
}

/// Multi-subscriber event bus with monotonic sequence numbers.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<Mutex<BusInner>>,
    lagged_events: Arc<AtomicU64>,
    persistence_error: Arc<Mutex<Option<String>>>,
    journal_gap: Arc<Mutex<Option<(u64, u64)>>>,
    /// Last sequence the durable writer has acked. 0 when persistence is off.
    durable_through: Arc<AtomicU64>,
    /// `with_persist_dir` was requested; RAM must not be served as durable.
    persist_configured: Arc<AtomicBool>,
}

#[derive(Clone)]
struct JournalSpace {
    mu: Arc<Mutex<()>>,
    cv: Arc<Condvar>,
}

struct PersistenceHandle {
    tx: Mutex<Option<SyncSender<JournalEntry>>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
    gap_path: PathBuf,
    /// Serializes durable enqueue so seq order is preserved after `inner` is released.
    order: Mutex<()>,
    space: JournalSpace,
}

struct JournalWriterCtx {
    path: PathBuf,
    gap_path: PathBuf,
    capacity: usize,
    persistence_error: Arc<Mutex<Option<String>>>,
    journal_gap: Arc<Mutex<Option<(u64, u64)>>>,
    space: JournalSpace,
    durable_through: Arc<AtomicU64>,
}

impl Drop for PersistenceHandle {
    fn drop(&mut self) {
        self.tx.lock().take();
        self.space.cv.notify_all();
        if let Some(join) = self.join.lock().take() {
            let _ = join.join();
        }
    }
}

pub struct EventReceiver {
    stream_rx: broadcast::Receiver<SequencedUpdate>,
    critical_rx: broadcast::Receiver<SequencedUpdate>,
    pending_stream: Option<SequencedUpdate>,
    pending_critical: Option<SequencedUpdate>,
    pending_lagged: u64,
    stream_closed: bool,
    critical_closed: bool,
    lagged_events: Arc<AtomicU64>,
}

#[derive(Clone)]
struct SequencedUpdate {
    seq: u64,
    update: SessionUpdate,
}

impl EventReceiver {
    pub async fn recv(&mut self) -> Option<SessionUpdate> {
        self.recv_with_seq().await.map(|(_, update)| update)
    }

    /// Receive the next redacted update together with its durable journal
    /// sequence. Transport adapters use this to assign resumable event IDs.
    pub async fn recv_with_seq(&mut self) -> Option<(u64, SessionUpdate)> {
        loop {
            match self.try_recv_with_seq() {
                Ok(update) => return Some(update),
                Err(broadcast::error::TryRecvError::Closed) => return None,
                Err(broadcast::error::TryRecvError::Lagged(_)) => unreachable!(),
                Err(broadcast::error::TryRecvError::Empty) => {}
            }

            tokio::select! {
                result = self.stream_rx.recv(), if !self.stream_closed => {
                    match result {
                        Ok(update) => self.pending_stream = Some(update),
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            self.lagged_events.fetch_add(count, Ordering::Relaxed);
                            self.pending_lagged = self.pending_lagged.saturating_add(count);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            self.stream_closed = true;
                        }
                    }
                }
                result = self.critical_rx.recv(), if !self.critical_closed => {
                    match result {
                        Ok(update) => self.pending_critical = Some(update),
                        Err(broadcast::error::RecvError::Lagged(count)) => {
                            self.lagged_events.fetch_add(count, Ordering::Relaxed);
                            self.pending_lagged = self.pending_lagged.saturating_add(count);
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            self.critical_closed = true;
                        }
                    }
                }
                else => return None,
            }
        }
    }

    pub fn try_recv(&mut self) -> Result<SessionUpdate, broadcast::error::TryRecvError> {
        self.try_recv_with_seq().map(|(_, update)| update)
    }

    pub fn try_recv_with_seq(
        &mut self,
    ) -> Result<(u64, SessionUpdate), broadcast::error::TryRecvError> {
        if let Some(gap) = self.take_gap_notice() {
            // A gap has no durable sequence of its own. The transport must
            // turn this into an explicit recovery signal, never a fake event.
            return Ok((0, gap));
        }
        self.fill_stream();
        self.fill_critical();
        if let Some(gap) = self.take_gap_notice() {
            return Ok((0, gap));
        }

        let take_stream = match (&self.pending_stream, &self.pending_critical) {
            (Some(stream), Some(critical)) => stream.seq < critical.seq,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => {
                return if self.stream_closed && self.critical_closed {
                    Err(broadcast::error::TryRecvError::Closed)
                } else {
                    Err(broadcast::error::TryRecvError::Empty)
                };
            }
        };
        let next = if take_stream {
            self.pending_stream.take()
        } else {
            self.pending_critical.take()
        };
        let next = next.expect("a pending event was selected");
        Ok((next.seq, next.update))
    }

    fn fill_stream(&mut self) {
        while self.pending_stream.is_none() && !self.stream_closed {
            match self.stream_rx.try_recv() {
                Ok(update) => self.pending_stream = Some(update),
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    self.lagged_events.fetch_add(count, Ordering::Relaxed);
                    self.pending_lagged = self.pending_lagged.saturating_add(count);
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => self.stream_closed = true,
            }
        }
    }

    fn fill_critical(&mut self) {
        while self.pending_critical.is_none() && !self.critical_closed {
            match self.critical_rx.try_recv() {
                Ok(update) => self.pending_critical = Some(update),
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    self.lagged_events.fetch_add(count, Ordering::Relaxed);
                    self.pending_lagged = self.pending_lagged.saturating_add(count);
                }
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(broadcast::error::TryRecvError::Closed) => self.critical_closed = true,
            }
        }
    }

    fn take_gap_notice(&mut self) -> Option<SessionUpdate> {
        if self.pending_lagged == 0 {
            return None;
        }
        let count = std::mem::take(&mut self.pending_lagged);
        Some(SessionUpdate::Error {
            session_id: uuid::Uuid::nil(),
            message: format!(
                "live event receiver missed {count} bounded backlog event(s); resynchronize from the durable journal"
            ),
        })
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (stream_tx, _) = broadcast::channel(capacity.max(1));
        let (critical_tx, _) = broadcast::channel(512);
        Self {
            inner: Arc::new(Mutex::new(BusInner {
                stream_tx,
                critical_tx,
                journal: VecDeque::new(),
                journal_bytes: 0,
                capacity: capacity.max(1),
                next_seq: 1,
                last_seq: 0,
                reserved_through: 0,
                sequence_path: None,
                oldest_seq: 1,
                persistence: None,
                control_secrets: Vec::new(),
            })),
            lagged_events: Arc::new(AtomicU64::new(0)),
            persistence_error: Arc::new(Mutex::new(None)),
            journal_gap: Arc::new(Mutex::new(None)),
            durable_through: Arc::new(AtomicU64::new(0)),
            persist_configured: Arc::new(AtomicBool::new(false)),
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

    pub fn redact_text(&self, text: &str, max: usize) -> String {
        let secrets = self.inner.lock().control_secrets.clone();
        scrub_text(text, &secrets, max)
    }

    /// Persist journal under `dir/event_journal.jsonl`; reload tail; compact file.
    pub fn with_persist_dir(self, dir: impl AsRef<Path>) -> Self {
        self.persist_configured.store(true, Ordering::Release);
        let path = dir.as_ref().join("event_journal.jsonl");
        let sequence_path = dir.as_ref().join("event_journal.seq");
        let gap_path = dir.as_ref().join("event_journal.gap.json");
        if let Some(parent) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(parent) {
                *self.persistence_error.lock() = Some(error.to_string());
            }
        }
        {
            let mut g = self.inner.lock();
            let mut durable_tail = VecDeque::new();
            let gap_ready = if gap_path.is_file() {
                match load_journal_gap(&gap_path) {
                    Ok(gap) => {
                        *self.journal_gap.lock() = Some(gap);
                        true
                    }
                    Err(error) => {
                        *self.persistence_error.lock() = Some(error.to_string());
                        *self.journal_gap.lock() = Some((0, u64::MAX));
                        false
                    }
                }
            } else {
                true
            };
            if path.is_file() {
                match load_journal_file(&path, g.capacity) {
                    Ok(loaded) => {
                        if let Some(last) = loaded.back() {
                            g.next_seq = last.seq.saturating_add(1);
                            g.last_seq = last.seq;
                            g.oldest_seq = loaded.front().map(|e| e.seq).unwrap_or(1);
                        }
                        g.journal_bytes = loaded.iter().map(journal_entry_size).sum();
                        g.journal = loaded.clone();
                        durable_tail = loaded;
                        self.durable_through.store(g.last_seq, Ordering::Release);
                        // Compact durable file to current in-memory tail.
                        if let Err(e) = rewrite_journal_file(&path, &g.journal) {
                            *self.persistence_error.lock() = Some(e.to_string());
                        }
                    }
                    Err(error) => {
                        *self.persistence_error.lock() = Some(error.to_string());
                    }
                }
            }
            let sequence_ready = match reserve_sequence_range(&sequence_path, g.next_seq) {
                Ok((start, reserved_through)) => {
                    g.next_seq = start;
                    g.reserved_through = reserved_through;
                    g.sequence_path = Some(sequence_path);
                    true
                }
                Err(error) => {
                    *self.persistence_error.lock() = Some(error.to_string());
                    g.reserved_through = u64::MAX;
                    g.sequence_path = None;
                    false
                }
            };
            if sequence_ready && gap_ready {
                let (tx, rx) = sync_channel::<JournalEntry>(256);
                let persistence_error = self.persistence_error.clone();
                let journal_gap = self.journal_gap.clone();
                let writer_path = path.clone();
                let writer_gap_path = gap_path.clone();
                let capacity = g.capacity;
                let space = JournalSpace {
                    mu: Arc::new(Mutex::new(())),
                    cv: Arc::new(Condvar::new()),
                };
                let writer_ctx = JournalWriterCtx {
                    path: writer_path,
                    gap_path: writer_gap_path,
                    capacity,
                    persistence_error,
                    journal_gap,
                    space: space.clone(),
                    durable_through: self.durable_through.clone(),
                };
                let join = std::thread::Builder::new()
                    .name("grokptah-event-journal".into())
                    .spawn(move || {
                        run_journal_writer(writer_ctx, durable_tail, rx);
                    });
                match join {
                    Ok(join) => {
                        g.persistence = Some(Arc::new(PersistenceHandle {
                            tx: Mutex::new(Some(tx)),
                            join: Mutex::new(Some(join)),
                            gap_path: gap_path.clone(),
                            order: Mutex::new(()),
                            space,
                        }));
                    }
                    Err(error) => {
                        *self.persistence_error.lock() = Some(error.to_string());
                    }
                }
            }
        }
        self
    }

    pub fn subscribe(&self) -> EventReceiver {
        let g = self.inner.lock();
        let stream_rx = g.stream_tx.subscribe();
        let critical_rx = g.critical_tx.subscribe();
        drop(g);
        EventReceiver {
            stream_rx,
            critical_rx,
            pending_stream: None,
            pending_critical: None,
            pending_lagged: 0,
            stream_closed: false,
            critical_closed: false,
            lagged_events: self.lagged_events.clone(),
        }
    }

    /// Publish: allocate seq + journal insert + fan-out under one lock (monotonic).
    pub fn publish(&self, update: SessionUpdate) {
        let mut g = self.inner.lock();
        if g.next_seq > g.reserved_through {
            if let Some(path) = g.sequence_path.clone() {
                match reserve_sequence_range(&path, g.next_seq) {
                    Ok((start, reserved_through)) => {
                        g.next_seq = start;
                        g.reserved_through = reserved_through;
                    }
                    Err(error) => {
                        *self.persistence_error.lock() = Some(error.to_string());
                        g.persistence.take();
                        g.sequence_path = None;
                        g.reserved_through = u64::MAX;
                    }
                }
            }
        }
        let seq = g.next_seq;
        g.next_seq = g.next_seq.saturating_add(1);
        g.last_seq = seq;
        let ts = chrono::Utc::now().to_rfc3339();
        let secrets = g.control_secrets.clone();
        let redacted = bound_update_size(redact_update_with_secrets(update, &secrets));
        let entry = JournalEntry {
            seq,
            ts,
            update: redacted.clone(),
        };
        g.journal_bytes = g.journal_bytes.saturating_add(journal_entry_size(&entry));
        g.journal.push_back(entry.clone());
        while g.journal.len() > g.capacity || g.journal_bytes > MAX_JOURNAL_BYTES {
            if let Some(old) = g.journal.pop_front() {
                g.journal_bytes = g.journal_bytes.saturating_sub(journal_entry_size(&old));
                g.oldest_seq = old.seq.saturating_add(1);
            }
        }
        if is_critical_update(&redacted) {
            let _ = g.critical_tx.send(SequencedUpdate {
                seq,
                update: redacted,
            });
        } else {
            let _ = g.stream_tx.send(SequencedUpdate {
                seq,
                update: redacted,
            });
        }
        if let Some(handle) = g.persistence.clone() {
            let persist_order = handle.order.lock();
            drop(g);
            let sender = handle.tx.lock().clone();
            let failure = match sender.as_ref() {
                Some(tx) => enqueue_journal_entry(tx, &entry, &handle.space),
                None => Some(("journal writer stopped", true)),
            };
            drop(persist_order);
            if let Some((detail, _)) = failure {
                *self.persistence_error.lock() = Some(detail.into());
                record_journal_gap(&self.journal_gap, seq);
                if let Err(error) = persist_current_gap(&handle.gap_path, &self.journal_gap) {
                    *self.persistence_error.lock() = Some(error.to_string());
                }
                self.inner.lock().persistence.take();
            }
        }
    }

    pub fn send(&self, update: SessionUpdate) -> Result<(), std::convert::Infallible> {
        self.publish(update);
        Ok(())
    }

    /// Cursor is exclusive: return entries with `seq > after_seq`.
    ///
    /// When persistence is configured this page is the durable prefix only.
    /// RAM ahead of [`Self::durable_through`] is never returned as a successful
    /// catch-up; the reader waits on the writer condvar, then either returns
    /// acked entries or marks `writer_pending`.
    pub fn read_after(&self, after_seq: u64, limit: usize) -> JournalPage {
        let limit = limit.clamp(1, 500);
        self.wait_for_durable(after_seq);
        let g = self.inner.lock();
        let gap = *self.journal_gap.lock();
        let expired = JournalPage {
            entries: Vec::new(),
            next_cursor: None,
            cursor_expired: true,
            writer_pending: false,
        };
        if let Some((start, end)) = gap {
            if after_seq >= start.saturating_sub(1) && after_seq < end {
                return expired;
            }
        }
        if after_seq > 0 && after_seq < g.oldest_seq.saturating_sub(1) && !g.journal.is_empty() {
            return expired;
        }
        let mut entries: Vec<JournalEntry> = g
            .journal
            .iter()
            .filter(|e| e.seq > after_seq)
            .cloned()
            .collect();
        if let Some((start, _)) = gap {
            if after_seq < start.saturating_sub(1) {
                entries.retain(|entry| entry.seq < start);
                if entries.is_empty() && !g.journal.is_empty() && g.oldest_seq >= start {
                    // Prefix was evicted and the persist gap is still ahead:
                    // fail closed rather than skip the hole or return an empty
                    // success page.
                    return expired;
                }
            }
        }
        let persist = self.persist_configured.load(Ordering::Acquire);
        let durable = self.durable_through.load(Ordering::Acquire);
        let unpublished = if persist {
            let before = entries.len();
            entries.retain(|entry| entry.seq <= durable);
            before > entries.len()
        } else {
            false
        };
        entries.truncate(limit);
        let writer_pending = persist && unpublished && entries.len() < limit;
        let next_cursor = entries.last().map(|e| e.seq);
        JournalPage {
            entries,
            next_cursor,
            cursor_expired: false,
            writer_pending,
        }
    }

    /// Wait until the durable writer has acked enough of the requested range
    /// or the bounded backpressure timeout elapses.
    fn wait_for_durable(&self, after_seq: u64) {
        if !self.persist_configured.load(Ordering::Acquire) {
            return;
        }
        let Some(handle) = self.inner.lock().persistence.clone() else {
            return;
        };
        let deadline = Instant::now() + WRITER_SEND_TIMEOUT;
        loop {
            if let Some((start, end)) = *self.journal_gap.lock() {
                if after_seq >= start.saturating_sub(1) && after_seq < end {
                    return;
                }
            }
            if self.durable_through.load(Ordering::Acquire) >= self.current_seq() {
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let mut guard = handle.space.mu.lock();
            if self.durable_through.load(Ordering::Acquire) >= self.current_seq() {
                return;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            handle.space.cv.wait_for(&mut guard, remaining);
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
                // An empty page with writer_pending is not EOF. Callers must
                // fail closed or retry rather than treat this as catch-up.
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
        self.inner.lock().last_seq
    }

    pub fn next_seq(&self) -> u64 {
        self.inner.lock().next_seq
    }

    pub fn oldest_seq(&self) -> u64 {
        self.inner.lock().oldest_seq
    }

    pub fn subscriber_count(&self) -> usize {
        let g = self.inner.lock();
        g.stream_tx
            .receiver_count()
            .max(g.critical_tx.receiver_count())
    }

    pub fn lagged_event_count(&self) -> u64 {
        self.lagged_events.load(Ordering::Relaxed)
    }

    pub fn last_persistence_error(&self) -> Option<String> {
        self.persistence_error.lock().clone()
    }

    /// Last sequence acked by the durable writer, or 0 when persistence is off.
    pub fn durable_through(&self) -> u64 {
        self.durable_through.load(Ordering::Acquire)
    }

    /// Whether this bus was asked to persist; RAM must not be treated as durable.
    pub fn persist_configured(&self) -> bool {
        self.persist_configured.load(Ordering::Acquire)
    }

    pub fn durable_writer_alive(&self) -> bool {
        self.inner.lock().persistence.is_some()
    }
}

fn is_critical_update(update: &SessionUpdate) -> bool {
    matches!(
        update,
        SessionUpdate::TurnStarted { .. }
            | SessionUpdate::CompletionEvidence { .. }
            | SessionUpdate::TurnComplete { .. }
            | SessionUpdate::Error { .. }
            | SessionUpdate::PermissionRequired { .. }
            | SessionUpdate::ShellSessionEnded { .. }
            | SessionUpdate::FileEdit { .. }
            | SessionUpdate::RateLimited { .. }
            | SessionUpdate::PromptQueueChanged { .. }
    )
}

fn journal_entry_size(entry: &JournalEntry) -> usize {
    serde_json::to_vec(entry)
        .map(|bytes| bytes.len().saturating_add(1))
        .unwrap_or(MAX_EVENT_BYTES)
}

fn bound_update_size(update: SessionUpdate) -> SessionUpdate {
    if serde_json::to_vec(&update)
        .map(|bytes| bytes.len() <= MAX_EVENT_BYTES)
        .unwrap_or(false)
    {
        return update;
    }
    let session_id = session_id_of(&update).unwrap_or_else(uuid::Uuid::nil);
    SessionUpdate::Error {
        session_id,
        message: "event omitted because its serialized payload exceeded 256 KiB".into(),
    }
}

#[derive(Debug)]
pub struct CursorExpiredError;

pub(crate) fn session_id_of(u: &SessionUpdate) -> Option<uuid::Uuid> {
    use SessionUpdate::*;
    match u {
        AgentMessageChunk { session_id, .. }
        | AgentThoughtChunk { session_id, .. }
        | TurnStarted { session_id, .. }
        | ToolCall { session_id, .. }
        | ToolCallUpdate { session_id, .. }
        | Plan { session_id, .. }
        | PermissionRequired { session_id, .. }
        | CompletionEvidence { session_id, .. }
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
        | SteeringInjected { session_id, .. }
        | PromptQueueChanged { session_id, .. }
        | ComputerApprovalRequired { session_id, .. } => Some(*session_id),
        BackgroundTask { session_id, .. } => *session_id,
    }
}

fn run_journal_writer(
    ctx: JournalWriterCtx,
    mut tail: VecDeque<JournalEntry>,
    rx: std::sync::mpsc::Receiver<JournalEntry>,
) {
    use std::sync::mpsc::RecvTimeoutError;

    let mut tail_bytes: usize = tail.iter().map(journal_entry_size).sum();
    let mut file_bytes = std::fs::metadata(&ctx.path)
        .map(|metadata| metadata.len() as usize)
        .unwrap_or(0);
    let mut persisted_gap = load_journal_gap(&ctx.gap_path).ok();
    loop {
        let entry = match rx.recv_timeout(std::time::Duration::from_millis(25)) {
            Ok(entry) => {
                let _guard = ctx.space.mu.lock();
                ctx.space.cv.notify_one();
                Some(entry)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => {
                let _guard = ctx.space.mu.lock();
                ctx.space.cv.notify_all();
                flush_journal_gap(
                    &ctx.gap_path,
                    &ctx.journal_gap,
                    &mut persisted_gap,
                    &ctx.persistence_error,
                );
                break;
            }
        };
        let Some(entry) = entry else {
            flush_journal_gap(
                &ctx.gap_path,
                &ctx.journal_gap,
                &mut persisted_gap,
                &ctx.persistence_error,
            );
            continue;
        };
        let entry_bytes = journal_entry_size(&entry);
        tail_bytes = tail_bytes.saturating_add(entry_bytes);
        tail.push_back(entry.clone());
        while tail.len() > ctx.capacity || tail_bytes > MAX_JOURNAL_BYTES {
            if let Some(old) = tail.pop_front() {
                tail_bytes = tail_bytes.saturating_sub(journal_entry_size(&old));
            }
        }
        let result = if file_bytes.saturating_add(entry_bytes) > MAX_JOURNAL_BYTES {
            rewrite_journal_file(&ctx.path, &tail).map(|_| {
                file_bytes = tail_bytes;
            })
        } else {
            append_journal_line(&ctx.path, &entry).map(|_| {
                file_bytes = file_bytes.saturating_add(entry_bytes);
            })
        };
        if let Err(error) = result {
            *ctx.persistence_error.lock() = Some(error.to_string());
            record_journal_gap(&ctx.journal_gap, entry.seq);
        } else {
            ctx.durable_through.store(entry.seq, Ordering::Release);
        }
        {
            let _guard = ctx.space.mu.lock();
            ctx.space.cv.notify_all();
        }
        flush_journal_gap(
            &ctx.gap_path,
            &ctx.journal_gap,
            &mut persisted_gap,
            &ctx.persistence_error,
        );
    }
}

fn enqueue_journal_entry(
    tx: &SyncSender<JournalEntry>,
    entry: &JournalEntry,
    space: &JournalSpace,
) -> Option<(&'static str, bool)> {
    let deadline = Instant::now() + WRITER_SEND_TIMEOUT;
    loop {
        match tx.try_send(entry.clone()) {
            Ok(()) => return None,
            Err(TrySendError::Disconnected(_)) => {
                return Some(("journal writer stopped", true));
            }
            Err(TrySendError::Full(_)) => {
                let mut guard = space.mu.lock();
                match tx.try_send(entry.clone()) {
                    Ok(()) => return None,
                    Err(TrySendError::Disconnected(_)) => {
                        return Some(("journal writer stopped", true));
                    }
                    Err(TrySendError::Full(_)) => {
                        let now = Instant::now();
                        if now >= deadline {
                            return Some(("journal writer backpressure timeout", false));
                        }
                        let remaining = deadline.saturating_duration_since(now);
                        if space.cv.wait_for(&mut guard, remaining).timed_out() {
                            match tx.try_send(entry.clone()) {
                                Ok(()) => return None,
                                Err(TrySendError::Disconnected(_)) => {
                                    return Some(("journal writer stopped", true));
                                }
                                Err(TrySendError::Full(_)) => {
                                    return Some(("journal writer backpressure timeout", false));
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn record_journal_gap(gap: &Mutex<Option<(u64, u64)>>, seq: u64) {
    let mut current = gap.lock();
    *current = Some(match *current {
        Some((start, end)) => (start.min(seq), end.max(seq)),
        None => (seq, seq),
    });
}

fn load_journal_gap(path: &Path) -> std::io::Result<(u64, u64)> {
    let text = std::fs::read_to_string(path)?;
    let gap: (u64, u64) = serde_json::from_str(&text).map_err(std::io::Error::other)?;
    if gap.0 > gap.1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "journal gap start exceeds end",
        ));
    }
    Ok(gap)
}

fn flush_journal_gap(
    path: &Path,
    journal_gap: &Mutex<Option<(u64, u64)>>,
    persisted_gap: &mut Option<(u64, u64)>,
    persistence_error: &Mutex<Option<String>>,
) {
    let current = *journal_gap.lock();
    if current.is_none() || current == *persisted_gap {
        return;
    }
    let Some(gap) = current else {
        return;
    };
    let result = serde_json::to_vec(&gap)
        .map_err(std::io::Error::other)
        .and_then(|bytes| atomic_write_bytes(path, &bytes));
    match result {
        Ok(()) => *persisted_gap = Some(gap),
        Err(error) => *persistence_error.lock() = Some(error.to_string()),
    }
}

fn persist_current_gap(
    path: &Path,
    journal_gap: &Mutex<Option<(u64, u64)>>,
) -> std::io::Result<()> {
    let Some(gap) = *journal_gap.lock() else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(&gap).map_err(std::io::Error::other)?;
    atomic_write_bytes(path, &bytes)
}

fn append_journal_line(path: &Path, entry: &JournalEntry) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
    writeln!(f, "{line}")
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

fn reserve_sequence_range(path: &Path, requested_start: u64) -> std::io::Result<(u64, u64)> {
    let previous = match std::fs::read_to_string(path) {
        Ok(text) => text.trim().parse::<u64>().map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("invalid sequence reservation: {error}"),
            )
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    let start = requested_start.max(previous.saturating_add(1));
    let reserved_through = start.saturating_add(SEQUENCE_RESERVATION_SIZE - 1);
    atomic_write_bytes(path, format!("{reserved_through}\n").as_bytes())?;
    Ok((start, reserved_through))
}

fn atomic_write_bytes(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = path.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn load_journal_file(path: &Path, capacity: usize) -> std::io::Result<VecDeque<JournalEntry>> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut all: VecDeque<JournalEntry> = VecDeque::new();
    let mut all_bytes = 0usize;
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !line.is_empty() && !oversized {
                push_loaded_journal_line(&line, capacity, &mut all, &mut all_bytes);
            }
            break;
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if !oversized {
                if line.len().saturating_add(newline) <= MAX_JOURNAL_LINE_BYTES {
                    line.extend_from_slice(&available[..newline]);
                } else {
                    oversized = true;
                }
            }
            reader.consume(newline + 1);
            if !oversized {
                push_loaded_journal_line(&line, capacity, &mut all, &mut all_bytes);
            }
            line.clear();
            oversized = false;
        } else {
            let available_len = available.len();
            if !oversized {
                if line.len().saturating_add(available_len) <= MAX_JOURNAL_LINE_BYTES {
                    line.extend_from_slice(available);
                } else {
                    oversized = true;
                    line.clear();
                }
            }
            reader.consume(available_len);
        }
    }
    Ok(all)
}

fn push_loaded_journal_line(
    line: &[u8],
    capacity: usize,
    all: &mut VecDeque<JournalEntry>,
    all_bytes: &mut usize,
) {
    if line.iter().all(u8::is_ascii_whitespace) {
        return;
    }
    if let Ok(entry) = serde_json::from_slice::<JournalEntry>(line) {
        *all_bytes = all_bytes.saturating_add(journal_entry_size(&entry));
        all.push_back(entry);
        while all.len() > capacity || *all_bytes > MAX_JOURNAL_BYTES {
            if let Some(old) = all.pop_front() {
                *all_bytes = all_bytes.saturating_sub(journal_entry_size(&old));
            }
        }
    }
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
        SessionUpdate::PromptQueueChanged {
            entries,
            changed_entry,
            action,
            origin,
            ..
        } => {
            // Queue text is an editable projection, not display-only chrome:
            // the desktop seeds its edit draft from this snapshot and saves it
            // back under the same entry version. A length cap here would let a
            // truncated copy overwrite the durable prompt and would give the
            // GUI different text than `ptah_get_queue` returns. Secrets are
            // still scrubbed; only the cap is dropped.
            for entry in entries {
                entry.text = scrub_secrets(&entry.text, control_secrets);
                entry.source = scrub_text(&entry.source, control_secrets, 200);
                if let Some(owner) = entry.owner.as_mut() {
                    *owner = scrub_text(owner, control_secrets, 200);
                }
            }
            if let Some(entry) = changed_entry {
                entry.text = scrub_secrets(&entry.text, control_secrets);
                entry.source = scrub_text(&entry.source, control_secrets, 200);
                if let Some(owner) = entry.owner.as_mut() {
                    *owner = scrub_text(owner, control_secrets, 200);
                }
            }
            *action = scrub_text(action, control_secrets, 100);
            *origin = scrub_text(origin, control_secrets, 100);
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
            steps.truncate(100);
            for s in steps.iter_mut() {
                *s = scrub_text(s, control_secrets, 500);
            }
        }
        SessionUpdate::TurnStarted { .. }
        | SessionUpdate::CompletionEvidence { .. }
        | SessionUpdate::TurnComplete { .. }
        | SessionUpdate::ShellSessionEnded { .. }
        | SessionUpdate::SubagentSpawned { .. }
        | SessionUpdate::ComputerApprovalRequired { .. } => {}
    }
    update
}

fn redact_json(v: &serde_json::Value, secrets: &[String]) -> serde_json::Value {
    redact_json_inner(v, secrets, 0)
}

fn redact_json_inner(v: &serde_json::Value, secrets: &[String], depth: usize) -> serde_json::Value {
    if depth >= 8 {
        return serde_json::json!("[truncated: depth limit]");
    }
    match v {
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map.iter().take(100) {
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
                    out.insert(k.clone(), redact_json_inner(val, secrets, depth + 1));
                }
            }
            if map.len() > 100 {
                out.insert(
                    "__truncated__".into(),
                    serde_json::json!(format!("{} fields omitted", map.len() - 100)),
                );
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.iter()
                .take(100)
                .map(|value| redact_json_inner(value, secrets, depth + 1))
                .collect(),
        ),
        serde_json::Value::String(s) => serde_json::Value::String(scrub_text(s, secrets, 2_000)),
        other => other.clone(),
    }
}

/// Remove bearer values and secrets entirely, preserving length.
///
/// Use this for text a client may write back (prompt queue entries). Anything
/// display-only should go through [`scrub_text`], which also caps length.
fn scrub_secrets(s: &str, secrets: &[String]) -> String {
    static BEARER: OnceLock<regex::Regex> = OnceLock::new();
    static AUTH: OnceLock<regex::Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<regex::Regex> = OnceLock::new();
    let bearer = BEARER.get_or_init(|| {
        regex::Regex::new(r#"(?i)\bbearer\s+[^\s"',;]+"#).expect("valid bearer redaction")
    });
    let auth = AUTH.get_or_init(|| {
        regex::Regex::new(r"(?im)authorization\s*:\s*[^\r\n]+")
            .expect("valid authorization redaction")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        regex::Regex::new(
            r#"(?i)\b[A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|PASSWD|API_KEY|PRIVATE_KEY|ACCESS_KEY)(?:_ID)?["']?\s*[:=]\s*(?:"[^"]*"|'[^']*'|[^\s"',;]+)"#,
        )
        .expect("valid assignment redaction")
    });
    let mut out = bearer.replace_all(s, "Bearer [redacted]").into_owned();
    out = auth
        .replace_all(&out, "authorization: [redacted]")
        .into_owned();
    out = assignment
        .replace_all(&out, "credential=[redacted]")
        .into_owned();
    for secret in secrets {
        if !secret.is_empty() && out.contains(secret) {
            out = scrub_registered_secret(&out, secret);
        }
    }
    out
}

/// Scrub secrets, then cap length for display-only fields.
fn scrub_text(s: &str, secrets: &[String], max: usize) -> String {
    let out = scrub_secrets(s, secrets);
    if out.len() <= max {
        out
    } else {
        let head = crate::textutil::truncate_at_char_boundary(&out, max);
        format!("{head}…[truncated {} bytes]", out.len())
    }
}

fn scrub_registered_secret(text: &str, secret: &str) -> String {
    if secret.len() >= 8 {
        return text.replace(secret, "[redacted]");
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(secret) {
        let start = cursor + relative;
        let end = start + secret.len();
        let before_is_word = start > 0 && text.as_bytes()[start - 1].is_ascii_alphanumeric();
        let after_is_word = end < text.len() && text.as_bytes()[end].is_ascii_alphanumeric();
        out.push_str(&text[cursor..start]);
        if before_is_word || after_is_word {
            out.push_str(secret);
        } else {
            out.push_str("[redacted]");
        }
        cursor = end;
    }
    out.push_str(&text[cursor..]);
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
    fn sequenced_receiver_exposes_durable_event_ids() {
        let bus = EventBus::new(8);
        let mut receiver = bus.subscribe();
        let sid = Uuid::new_v4();
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "first".into(),
        });
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "second".into(),
        });

        let (first_seq, _) = receiver.try_recv_with_seq().unwrap();
        let (second_seq, _) = receiver.try_recv_with_seq().unwrap();
        assert!(first_seq > 0);
        assert_eq!(second_seq, first_seq + 1);
    }

    #[tokio::test]
    async fn slow_subscriber_is_bounded_and_lag_is_observable() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..20 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("{i}"),
            });
        }
        let update = rx.recv().await.expect("gap notice");
        assert!(matches!(
            update,
            SessionUpdate::Error {
                session_id,
                ref message,
            } if session_id.is_nil() && message.contains("resynchronize")
        ));
        let update = rx.recv().await.expect("latest retained update");
        assert!(matches!(update, SessionUpdate::AgentMessageChunk { .. }));
        assert!(bus.lagged_event_count() > 0);
    }

    #[tokio::test]
    async fn terminal_event_survives_stream_flood() {
        let bus = EventBus::new(2);
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..100 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("{i}"),
            });
        }
        bus.publish(SessionUpdate::TurnComplete {
            session_id: sid,
            cancelled: false,
        });
        let mut found_terminal = false;
        for _ in 0..4 {
            if matches!(rx.recv().await, Some(SessionUpdate::TurnComplete { .. })) {
                found_terminal = true;
                break;
            }
        }
        assert!(found_terminal);
    }

    #[tokio::test]
    async fn critical_overflow_surfaces_receiver_gap_notice() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..600 {
            bus.publish(SessionUpdate::Error {
                session_id: sid,
                message: format!("critical-{i}"),
            });
        }
        assert!(matches!(
            rx.recv().await,
            Some(SessionUpdate::Error {
                session_id,
                ref message,
            }) if session_id.is_nil() && message.contains("resynchronize")
        ));
    }

    #[test]
    fn critical_and_stream_events_rejoin_in_publish_order() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "before".into(),
        });
        bus.publish(SessionUpdate::TurnComplete {
            session_id: sid,
            cancelled: false,
        });
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "after".into(),
        });

        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::AgentMessageChunk { ref text, .. }) if text == "before"
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::TurnComplete { .. })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::AgentMessageChunk { ref text, .. }) if text == "after"
        ));
    }

    #[test]
    fn turn_started_is_critical_and_rejoins_in_publish_order() {
        let bus = EventBus::new(8);
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        let turn_id = Uuid::new_v4();
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "before".into(),
        });
        bus.publish(SessionUpdate::TurnStarted {
            session_id: sid,
            turn_id,
        });
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "after".into(),
        });

        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::AgentMessageChunk { ref text, .. }) if text == "before"
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::TurnStarted {
                session_id,
                turn_id: received_turn_id,
            }) if session_id == sid && received_turn_id == turn_id
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(SessionUpdate::AgentMessageChunk { ref text, .. }) if text == "after"
        ));
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
    fn redact_prompt_queue_snapshot_without_losing_state() {
        let sid = Uuid::new_v4();
        let mut entry = crate::prompt_queue::PromptQueueEntry::new(
            "use ctl-token-abc123 only as a test",
            "control",
            false,
        )
        .unwrap();
        entry.owner = Some("ctl-token-abc123".into());
        let update = SessionUpdate::PromptQueueChanged {
            session_id: sid,
            revision: 7,
            entries: vec![entry.clone()],
            action: "queued".into(),
            origin: "mcp-secret-origin".into(),
            changed_entry: Some(entry),
            disposition: None,
        };
        let redacted = redact_update_with_secrets(update, &["ctl-token-abc123".into()]);
        match redacted {
            SessionUpdate::PromptQueueChanged {
                entries,
                changed_entry,
                action,
                origin,
                ..
            } => {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0].id, changed_entry.as_ref().unwrap().id);
                assert!(!entries[0].text.contains("ctl-token-abc123"));
                assert!(!entries[0]
                    .owner
                    .as_deref()
                    .unwrap()
                    .contains("ctl-token-abc123"));
                assert_eq!(action, "queued");
                assert_eq!(origin, "mcp-secret-origin");
            }
            _ => panic!("wrong variant"),
        }
    }

    /// S1: the desktop seeds its edit draft from this snapshot and saves it
    /// back under the same entry version, so a length cap here would let a
    /// truncated copy overwrite the durable prompt — and would give the GUI
    /// different text than `ptah_get_queue` hands an MCP coordinator.
    #[test]
    fn redact_preserves_full_queue_text_so_gui_and_mcp_agree() {
        let sid = Uuid::new_v4();
        // Comfortably past the old 4 KB cap and near MAX_PROMPT_BYTES.
        let long_text = "x".repeat(90_000);
        let entry =
            crate::prompt_queue::PromptQueueEntry::new(long_text.clone(), "composer", false)
                .unwrap();
        let update = SessionUpdate::PromptQueueChanged {
            session_id: sid,
            revision: 1,
            entries: vec![entry.clone()],
            action: "queued".into(),
            origin: "desktop".into(),
            changed_entry: Some(entry),
            disposition: None,
        };
        let redacted = redact_update_with_secrets(update, &["ctl-token-abc123".into()]);
        match redacted {
            SessionUpdate::PromptQueueChanged {
                entries,
                changed_entry,
                ..
            } => {
                assert_eq!(entries[0].text, long_text, "queue text must not truncate");
                assert!(!entries[0].text.contains("[truncated"));
                assert_eq!(changed_entry.unwrap().text, long_text);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// Dropping the cap must not drop the scrubbing: a secret buried past the
    /// old truncation point still has to be removed.
    #[test]
    fn redact_scrubs_secrets_beyond_the_old_queue_cap() {
        let sid = Uuid::new_v4();
        let secret = "ctl-token-abc123".to_string();
        let text = format!("{}\nuse {secret} here", "y".repeat(20_000));
        let entry = crate::prompt_queue::PromptQueueEntry::new(text, "composer", false).unwrap();
        let update = SessionUpdate::PromptQueueChanged {
            session_id: sid,
            revision: 1,
            entries: vec![entry.clone()],
            action: "queued".into(),
            origin: "desktop".into(),
            changed_entry: Some(entry),
            disposition: None,
        };
        let redacted = redact_update_with_secrets(update, std::slice::from_ref(&secret));
        match redacted {
            SessionUpdate::PromptQueueChanged { entries, .. } => {
                assert!(!entries[0].text.contains(&secret));
                assert!(entries[0].text.contains("[redacted]"));
                assert!(entries[0].text.len() > 20_000);
            }
            _ => panic!("wrong variant"),
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
    fn quoted_credentials_are_scrubbed() {
        let text = scrub_text(
            "OPENAI_API_KEY=\"secret-one\" XAI_API_KEY='secret-two' \
             AWS_SECRET_ACCESS_KEY=\"secret-three\" \"GITHUB_TOKEN\": 'secret-four' \
             PASSWORD=\"secret-five\"",
            &[],
            1_000,
        );
        assert!(!text.contains("secret-one"));
        assert!(!text.contains("secret-two"));
        assert!(!text.contains("secret-three"));
        assert!(!text.contains("secret-four"));
        assert!(!text.contains("secret-five"));
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
        assert!(bus2.current_seq() > seq_before);
    }

    #[test]
    fn restart_never_reuses_a_reserved_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let sid = Uuid::new_v4();
        let bus1 = EventBus::new(8).with_persist_dir(dir.path());
        bus1.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "first".into(),
        });
        let reserved_through = std::fs::read_to_string(dir.path().join("event_journal.seq"))
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap();
        drop(bus1);

        let bus2 = EventBus::new(8).with_persist_dir(dir.path());
        bus2.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "after restart".into(),
        });
        assert!(bus2.current_seq() > reserved_through);
    }

    #[test]
    fn corrupt_sequence_reservation_disables_durable_publication() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("event_journal.seq"), b"not-a-number").unwrap();
        let bus = EventBus::new(8).with_persist_dir(dir.path());
        bus.publish(SessionUpdate::TurnComplete {
            session_id: Uuid::new_v4(),
            cancelled: false,
        });
        assert!(bus.last_persistence_error().is_some());
        drop(bus);
        assert!(!dir.path().join("event_journal.jsonl").is_file());
    }

    #[test]
    fn corrupt_gap_metadata_fails_closed_for_replay_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("event_journal.gap.json"), b"{broken").unwrap();
        let bus = EventBus::new(8).with_persist_dir(dir.path());
        assert!(bus.read_after(0, 8).cursor_expired);
        bus.publish(SessionUpdate::TurnComplete {
            session_id: Uuid::new_v4(),
            cancelled: false,
        });
        drop(bus);
        assert!(!dir.path().join("event_journal.jsonl").is_file());
    }

    #[test]
    fn stopped_writer_persists_gap_on_exceptional_publish_path() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8).with_persist_dir(dir.path());
        let persistence = bus.inner.lock().persistence.clone().expect("writer handle");
        persistence.tx.lock().take();
        if let Some(join) = persistence.join.lock().take() {
            join.join().unwrap();
        }
        bus.publish(SessionUpdate::TurnComplete {
            session_id: Uuid::new_v4(),
            cancelled: false,
        });
        drop(bus);

        let gap: (u64, u64) = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("event_journal.gap.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(gap.0, gap.1);
    }

    #[test]
    fn durable_gap_marker_stops_replay_at_missing_range() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8).with_persist_dir(dir.path());
        record_journal_gap(&bus.journal_gap, 7);
        drop(bus);

        let reopened = EventBus::new(8).with_persist_dir(dir.path());
        assert!(reopened.read_after(6, 8).cursor_expired);
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

    #[test]
    fn persistence_setup_failure_is_observable() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"x").unwrap();
        let bus = EventBus::new(8).with_persist_dir(&blocker);
        assert!(bus.last_persistence_error().is_some());
    }

    #[test]
    fn oversized_journal_line_is_discarded_without_hiding_later_entries() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event_journal.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&vec![b'x'; MAX_JOURNAL_LINE_BYTES + 1])
            .unwrap();
        file.write_all(b"\n").unwrap();
        let sid = Uuid::new_v4();
        let valid = JournalEntry {
            seq: 42,
            ts: chrono::Utc::now().to_rfc3339(),
            update: SessionUpdate::TurnComplete {
                session_id: sid,
                cancelled: false,
            },
        };
        writeln!(file, "{}", serde_json::to_string(&valid).unwrap()).unwrap();
        drop(file);

        let bus = EventBus::new(8).with_persist_dir(dir.path());
        let page = bus.read_after(0, 8);
        assert_eq!(page.entries.len(), 1);
        assert_eq!(page.entries[0].seq, 42);
    }

    #[tokio::test]
    async fn hosted_shape_flood_wrap_then_durable_read_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(DEFAULT_JOURNAL_CAPACITY).with_persist_dir(dir.path());
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..6_000 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("flood-{i}"),
            });
        }
        let notice = rx.recv().await.expect("gap notice");
        assert!(matches!(
            notice,
            SessionUpdate::Error { ref message, .. } if message.contains("resynchronize")
        ));
        let page = bus.read_after(0, 500);
        assert!(!page.cursor_expired, "{page:?}");
        assert!(!page.entries.is_empty());
        assert!(page.entries.len() <= 500);
        assert!(bus.journal_gap.lock().is_none());
        assert_eq!(bus.last_persistence_error(), None);
    }

    #[test]
    fn cursor_after_known_gap_end_reads_the_retained_tail() {
        let bus = EventBus::new(32);
        let sid = Uuid::new_v4();
        for i in 0..8 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("g-{i}"),
            });
        }
        record_journal_gap(&bus.journal_gap, 3);
        let page = bus.read_after(3, 8);
        assert!(!page.cursor_expired, "{page:?}");
        assert_eq!(page.entries[0].seq, 4);
    }

    #[tokio::test]
    async fn lag_notice_then_durable_read_returns_retained_entries() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(64).with_persist_dir(dir.path());
        let mut rx = bus.subscribe();
        let sid = Uuid::new_v4();
        for i in 0..512 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("flood-{i}"),
            });
        }
        let notice = rx.recv().await.expect("gap notice");
        assert!(matches!(
            notice,
            SessionUpdate::Error { ref message, .. } if message.contains("resynchronize")
        ));
        let page = bus.read_after(0, 500);
        assert!(!page.cursor_expired, "{page:?}");
        assert!(!page.entries.is_empty());
        assert!(page.entries.len() <= 500);
        assert_eq!(bus.last_persistence_error(), None);
    }

    #[test]
    fn writer_backpressure_does_not_invent_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(32).with_persist_dir(dir.path());
        let sid = Uuid::new_v4();
        for i in 0..400 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("bp-{i}"),
            });
        }
        assert!(bus.journal_gap.lock().is_none());
        let page = bus.read_after(0, 500);
        assert!(!page.cursor_expired);
        assert_eq!(page.entries.len(), 32);
        assert!(bus.inner.lock().persistence.is_some());
    }

    #[test]
    fn persist_gap_after_wrap_fails_closed_instead_of_empty_success() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(8).with_persist_dir(dir.path());
        let sid = Uuid::new_v4();
        for i in 0..16 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("wrap-{i}"),
            });
        }
        record_journal_gap(&bus.journal_gap, 3);
        let page = bus.read_after(0, 500);
        assert!(page.cursor_expired);
        assert!(page.entries.is_empty());
    }

    #[test]
    fn restart_without_a_gap_replays_retained_seq() {
        let dir = tempfile::tempdir().unwrap();
        let sid = Uuid::new_v4();
        let bus1 = EventBus::new(16).with_persist_dir(dir.path());
        for i in 0..4 {
            bus1.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("ok-{i}"),
            });
        }
        drop(bus1);
        let bus2 = EventBus::new(16).with_persist_dir(dir.path());
        let page = bus2.read_after(0, 500);
        assert!(!page.cursor_expired);
        assert_eq!(page.entries.len(), 4);
        assert!(bus2.journal_gap.lock().is_none());
    }

    #[test]
    fn five_hundred_entry_pages_do_not_skip_the_run() {
        let bus = EventBus::new(2_000);
        let keep = Uuid::new_v4();
        let other = Uuid::new_v4();
        for i in 0..600 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: if i % 2 == 0 { keep } else { other },
                text: format!("{i}"),
            });
        }
        let first = bus.read_after(0, 500);
        assert_eq!(first.entries.len(), 500);
        assert!(!first.cursor_expired);
        let rest = bus.read_after(first.next_cursor.unwrap(), 500);
        assert_eq!(rest.entries.len(), 100);
        let mut seen = std::collections::BTreeSet::new();
        for e in first.entries.iter().chain(rest.entries.iter()) {
            assert!(seen.insert(e.seq));
        }
        assert_eq!(seen.len(), 600);
    }

    #[test]
    fn session_filter_does_not_advance_past_own_events() {
        let bus = EventBus::new(64);
        let keep = Uuid::new_v4();
        let other = Uuid::new_v4();
        for i in 0..40 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: other,
                text: format!("other-{i}"),
            });
        }
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: keep,
            text: "mine".into(),
        });
        let page = bus
            .read_range_all(0, None, Some(keep))
            .expect("own events must remain reachable");
        assert_eq!(page.len(), 1);
        assert!(
            matches!(page[0].update, SessionUpdate::AgentMessageChunk { ref text, .. } if text == "mine")
        );
    }

    #[test]
    fn restart_after_gap_preserves_monotonic_seq_and_gap_truth() {
        let dir = tempfile::tempdir().unwrap();
        let sid = Uuid::new_v4();
        let bus1 = EventBus::new(16).with_persist_dir(dir.path());
        for i in 0..4 {
            bus1.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("pre-{i}"),
            });
        }
        record_journal_gap(&bus1.journal_gap, 2);
        persist_current_gap(
            &dir.path().join("event_journal.gap.json"),
            &bus1.journal_gap,
        )
        .unwrap();
        let seq_before = bus1.current_seq();
        drop(bus1);

        let bus2 = EventBus::new(16).with_persist_dir(dir.path());
        assert!(bus2.read_after(1, 8).cursor_expired);
        bus2.publish(SessionUpdate::TurnComplete {
            session_id: sid,
            cancelled: false,
        });
        assert!(bus2.current_seq() > seq_before);
        let terminal = bus2
            .read_range_all(seq_before, None, Some(sid))
            .expect("post-gap terminal events remain readable");
        assert!(terminal
            .iter()
            .any(|e| matches!(e.update, SessionUpdate::TurnComplete { .. })));
        assert!(bus2.read_after(1, 8).cursor_expired);
    }

    #[test]
    fn concurrent_persisted_publish_has_no_duplicate_or_silent_loss() {
        use std::sync::Arc;
        use std::thread;
        let dir = tempfile::tempdir().unwrap();
        let bus = Arc::new(EventBus::new(2_000).with_persist_dir(dir.path()));
        let sid = Uuid::new_v4();
        let mut handles = Vec::new();
        for t in 0..4 {
            let bus = bus.clone();
            handles.push(thread::spawn(move || {
                for i in 0..40 {
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
        drop(bus);
        let reopened = EventBus::new(2_000).with_persist_dir(dir.path());
        let page = reopened
            .read_range_all(0, None, None)
            .expect("persisted concurrent run must remain replayable");
        assert_eq!(page.len(), 160);
        let mut seen = std::collections::BTreeSet::new();
        let mut last = 0u64;
        for e in &page {
            assert!(e.seq > last, "seq not monotonic: {} then {}", last, e.seq);
            last = e.seq;
            assert!(seen.insert(e.seq));
        }
        assert!(reopened.journal_gap.lock().is_none());
        assert_eq!(reopened.last_persistence_error(), None);
    }

    #[test]
    fn durable_read_only_returns_writer_acked_seqs() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(64).with_persist_dir(dir.path());
        let sid = Uuid::new_v4();
        for i in 0..24 {
            bus.publish(SessionUpdate::AgentMessageChunk {
                session_id: sid,
                text: format!("ack-{i}"),
            });
        }
        let page = bus.read_after(0, 500);
        assert!(!page.cursor_expired, "{page:?}");
        assert!(!page.writer_pending, "{page:?}");
        assert_eq!(page.entries.len(), 24);
        assert_eq!(bus.durable_through(), 24);
        let disk = std::fs::read_to_string(dir.path().join("event_journal.jsonl")).unwrap();
        for entry in &page.entries {
            assert!(
                disk.contains(&format!("\"seq\":{}", entry.seq)),
                "seq {} missing from durable file",
                entry.seq
            );
        }
        assert_eq!(
            page.entries.last().map(|entry| entry.seq),
            Some(bus.durable_through())
        );
    }

    #[test]
    fn durable_read_does_not_present_ram_ahead_of_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(32).with_persist_dir(dir.path());
        let sid = Uuid::new_v4();
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: "first".into(),
        });
        let _ = bus.read_after(0, 8);
        bus.durable_through.store(0, Ordering::Release);
        let page = bus.read_after(0, 8);
        assert!(!page.cursor_expired);
        assert!(page.entries.is_empty(), "{page:?}");
        assert!(page.writer_pending);
    }
}
