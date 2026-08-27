//! Append-only, bounded event journal with cursor replay.
//!
//! One journal per run. Writes are line-delimited JSON and are flushed before
//! the caller is told the event happened, so a crash loses at most the event
//! that was mid-write — and that torn tail is discarded on the next open rather
//! than parsed as truth.
//!
//! Retention is a real bound, not a display limit: once a run exceeds its
//! retention window the oldest events are compacted away and a cursor into that
//! region is reported as expired instead of being silently answered with a gap.

use std::io::Write;
use std::path::{Path, PathBuf};

use grokptah_agent_sdk::run::RunEvent;
use grokptah_agent_sdk::{ErrorEventRange, RunEventPage};
use serde_json::Value;

use crate::error::{HostError, HostResult, io_error};

/// Maximum entries returned in one page.
pub const MAX_PAGE_ENTRIES: usize = 128;

/// Whether a caller's cursor can still be answered exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStatus {
    /// The cursor is inside the retained window.
    Exact,
    /// Events after the cursor were compacted away.
    Expired,
    /// The cursor names a sequence this journal never produced.
    Ahead,
}

/// One run's durable event journal.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    entries: Vec<RunEvent>,
    next_seq: u64,
    retention: usize,
    truncated_tail: bool,
}

impl Journal {
    /// Open or create a journal, discarding a torn trailing write.
    ///
    /// A malformed line anywhere but the tail is treated as corruption and
    /// fails closed: silently skipping it would publish a journal with a hole
    /// while still claiming exact replay.
    pub fn open(path: &Path, retention: usize) -> HostResult<Self> {
        let retention = retention.max(1);
        let mut journal = Self {
            path: path.to_path_buf(),
            entries: Vec::new(),
            next_seq: 1,
            retention,
            truncated_tail: false,
        };

        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(io_error("journal_unreadable", &error)),
        };
        if raw.is_empty() {
            return Ok(journal);
        }

        let complete_len = raw.rfind('\n').map_or(0, |index| index + 1);
        if complete_len != raw.len() {
            journal.truncated_tail = true;
        }
        let complete = &raw[..complete_len];

        let total = complete.lines().count();
        for (index, line) in complete.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<RunEvent>(line) {
                Ok(event) => {
                    if event.seq < journal.next_seq {
                        return Err(HostError::internal(
                            "journal_corrupt",
                            "journal sequences are not strictly increasing",
                        ));
                    }
                    journal.next_seq = event.seq + 1;
                    journal.entries.push(event);
                }
                Err(_) if index + 1 == total => {
                    // Last complete line was still a partial record.
                    journal.truncated_tail = true;
                }
                Err(_) => {
                    return Err(HostError::internal(
                        "journal_corrupt",
                        "journal contains an unreadable record",
                    ));
                }
            }
        }

        if journal.truncated_tail {
            journal.rewrite()?;
        }
        Ok(journal)
    }

    /// Whether the last open discarded a torn trailing write.
    pub fn truncated_tail(&self) -> bool {
        self.truncated_tail
    }

    /// Sequence assigned to the next appended event.
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// Retained sequence window, if any events are retained.
    pub fn retained_range(&self) -> Option<ErrorEventRange> {
        let first = self.entries.first()?;
        let last = self.entries.last()?;
        Some(ErrorEventRange {
            start_seq: first.seq,
            end_seq: last.seq,
        })
    }

    /// Append one already-redacted update and flush it.
    pub fn append(&mut self, ts: String, update: Value) -> HostResult<RunEvent> {
        let event = RunEvent {
            seq: self.next_seq,
            ts,
            update,
        };
        event.validate().map_err(|reason| {
            HostError::invalid(
                "event_rejected",
                format!("event is not publishable: {reason}"),
            )
        })?;

        let mut line = serde_json::to_string(&event).map_err(|_| {
            HostError::internal("event_unserializable", "event cannot be journaled")
        })?;
        line.push('\n');

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| io_error("journal_unwritable", &error))?;
        file.write_all(line.as_bytes())
            .map_err(|error| io_error("journal_unwritable", &error))?;
        file.sync_data()
            .map_err(|error| io_error("journal_unwritable", &error))?;

        self.next_seq += 1;
        self.entries.push(event.clone());
        self.compact_if_needed()?;
        Ok(event)
    }

    /// Classify a caller cursor against the retained window.
    pub fn cursor_status(&self, after_seq: Option<u64>) -> CursorStatus {
        let after_seq = after_seq.unwrap_or(0);
        if after_seq + 1 > self.next_seq {
            return CursorStatus::Ahead;
        }
        match self.entries.first() {
            Some(first) if after_seq + 1 < first.seq => CursorStatus::Expired,
            _ => CursorStatus::Exact,
        }
    }

    /// Page retained events after a cursor.
    pub fn page(&self, after_seq: Option<u64>, limit: usize) -> RunEventPage {
        let status = self.cursor_status(after_seq);
        let after_seq = after_seq.unwrap_or(0);
        let limit = limit.clamp(1, MAX_PAGE_ENTRIES);

        let entries: Vec<RunEvent> = self
            .entries
            .iter()
            .filter(|event| event.seq > after_seq)
            .take(limit)
            .cloned()
            .collect();

        let last_returned = entries.last().map(|event| event.seq);
        let more_available = last_returned
            .zip(self.entries.last().map(|event| event.seq))
            .is_some_and(|(returned, newest)| returned < newest);

        RunEventPage {
            entries,
            next_cursor: if more_available { last_returned } else { None },
            cursor_expired: status != CursorStatus::Exact,
        }
    }

    fn compact_if_needed(&mut self) -> HostResult<()> {
        let high_water = self.retention + self.retention.div_ceil(2);
        if self.entries.len() <= high_water {
            return Ok(());
        }
        let drop_count = self.entries.len() - self.retention;
        self.entries.drain(..drop_count);
        self.rewrite()
    }

    fn rewrite(&self) -> HostResult<()> {
        let temp = self.path.with_extension("jsonl.tmp");
        let mut buffer = String::new();
        for event in &self.entries {
            let line = serde_json::to_string(event).map_err(|_| {
                HostError::internal("event_unserializable", "event cannot be journaled")
            })?;
            buffer.push_str(&line);
            buffer.push('\n');
        }
        crate::store::write_atomic(&temp, &self.path, buffer.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn journal(dir: &Path, retention: usize) -> Journal {
        Journal::open(&dir.join("events.jsonl"), retention).expect("journal opens")
    }

    #[test]
    fn events_replay_in_order_with_an_exact_cursor() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = journal(dir.path(), 64);
        for index in 0..5 {
            log.append("2026-01-01T00:00:00.000Z".into(), json!({ "step": index }))
                .expect("append");
        }

        let page = log.page(None, 3);
        assert_eq!(page.entries.len(), 3);
        assert_eq!(page.entries[0].seq, 1);
        assert!(!page.cursor_expired);
        assert_eq!(page.next_cursor, Some(3));

        let rest = log.page(page.next_cursor, 10);
        assert_eq!(rest.entries.len(), 2);
        assert_eq!(rest.next_cursor, None);
    }

    #[test]
    fn a_torn_trailing_write_is_discarded_not_parsed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        {
            let mut log = Journal::open(&path, 64).expect("journal opens");
            log.append("2026-01-01T00:00:00.000Z".into(), json!({ "step": 1 }))
                .expect("append");
        }
        let mut raw = std::fs::read_to_string(&path).expect("read");
        raw.push_str("{\"seq\":2,\"ts\":\"2026-01-01T00:00:00.000Z\",\"upda");
        std::fs::write(&path, raw).expect("write torn tail");

        let reopened = Journal::open(&path, 64).expect("journal reopens");
        assert!(reopened.truncated_tail());
        assert_eq!(reopened.next_seq(), 2);
        assert_eq!(reopened.page(None, 10).entries.len(), 1);

        let on_disk = std::fs::read_to_string(&path).expect("read");
        assert!(on_disk.ends_with('\n'));
        assert_eq!(on_disk.lines().count(), 1);
    }

    #[test]
    fn corruption_in_the_middle_fails_closed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, "not json\n{\"seq\":2,\"ts\":\"t\",\"update\":{}}\n").expect("write");
        let error = Journal::open(&path, 64).expect_err("corrupt journal is refused");
        assert_eq!(error.reason_code(), "journal_corrupt");
    }

    #[test]
    fn compaction_expires_a_cursor_instead_of_answering_with_a_gap() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = journal(dir.path(), 4);
        for index in 0..12 {
            log.append("2026-01-01T00:00:00.000Z".into(), json!({ "step": index }))
                .expect("append");
        }

        let range = log.retained_range().expect("range");
        assert!(range.start_seq > 1, "old events must be compacted away");
        assert_eq!(log.cursor_status(Some(1)), CursorStatus::Expired);
        assert!(log.page(Some(1), 10).cursor_expired);
        assert_eq!(log.cursor_status(Some(range.end_seq)), CursorStatus::Exact);
        assert_eq!(
            log.cursor_status(Some(range.end_seq + 5)),
            CursorStatus::Ahead
        );
    }

    #[test]
    fn a_compacted_journal_survives_reopen() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("events.jsonl");
        {
            let mut log = Journal::open(&path, 4).expect("journal opens");
            for index in 0..12 {
                log.append("2026-01-01T00:00:00.000Z".into(), json!({ "step": index }))
                    .expect("append");
            }
        }
        let reopened = Journal::open(&path, 4).expect("journal reopens");
        assert_eq!(reopened.next_seq(), 13);
        assert!(!reopened.truncated_tail());
        assert_eq!(
            reopened.retained_range().expect("range").end_seq,
            12,
            "the newest event must survive compaction"
        );
    }
}
