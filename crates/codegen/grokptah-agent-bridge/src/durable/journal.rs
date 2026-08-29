//! Bounded, crash-honest journal scanning.
//!
//! `main` reads durable records with `let Ok(record) = serde_json::from_str(..)
//! else { continue }`, so a corrupt or truncated record is skipped in silence.
//! Silence is the problem: an operator cannot tell a clean journal from one
//! that lost half its records, and a malformed-record loop has no bound.
//!
//! This scanner counts what it could not read, distinguishes a crash-cut tail
//! from corruption in the middle, and stops at explicit bounds rather than
//! scanning an unbounded file.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// Maximum records returned from one scan.
pub const MAX_JOURNAL_RECORDS: usize = 10_000;
/// Maximum bytes accepted for one record.
pub const MAX_RECORD_BYTES: usize = 1_000_000;
/// Malformed records tolerated before a scan gives up and reports.
pub const MAX_MALFORMED_RECORDS: usize = 64;

/// What a scan found, beyond the records themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub accepted: usize,
    /// Records that parsed as neither valid JSON nor a known shape.
    pub malformed: usize,
    /// Records that exceeded [`MAX_RECORD_BYTES`].
    pub oversized: usize,
    /// A final line with no terminating newline: the signature of a crash
    /// during an append, not of corruption.
    pub truncated_tail: bool,
    /// The scan stopped at a bound rather than at the end of the input.
    pub bounded: bool,
    /// The scan stopped because malformed records exceeded the tolerance.
    pub abandoned_on_malformed: bool,
}

impl ScanReport {
    /// Whether the journal can be treated as a complete record of the past.
    pub fn is_clean(&self) -> bool {
        self.malformed == 0
            && self.oversized == 0
            && !self.truncated_tail
            && !self.bounded
            && !self.abandoned_on_malformed
    }

    /// Operator-facing summary. Host-authored text only.
    pub fn operator_summary(&self) -> String {
        if self.is_clean() {
            return format!("{} records, journal clean", self.accepted);
        }
        let mut parts = vec![format!("{} records", self.accepted)];
        if self.malformed > 0 {
            parts.push(format!("{} malformed", self.malformed));
        }
        if self.oversized > 0 {
            parts.push(format!("{} oversized", self.oversized));
        }
        if self.truncated_tail {
            parts.push("truncated tail (crash during append)".to_string());
        }
        if self.bounded {
            parts.push("stopped at scan bound".to_string());
        }
        if self.abandoned_on_malformed {
            parts.push("abandoned: too many malformed records".to_string());
        }
        parts.join(", ")
    }
}

/// A scan's records plus its honest account of what it could not read.
#[derive(Clone, Debug)]
pub struct Scan<T> {
    pub records: Vec<T>,
    pub report: ScanReport,
}

/// Scan newline-delimited JSON.
///
/// A trailing line without a newline is reported as `truncated_tail` and is not
/// counted as malformed: the two have different causes and different remedies,
/// and conflating them makes a normal crash look like corruption.
pub fn scan_ndjson<T: DeserializeOwned>(input: &str) -> Scan<T> {
    let mut report = ScanReport::default();
    let mut records = Vec::new();

    let ends_with_newline = input.is_empty() || input.ends_with('\n');
    let mut lines: Vec<&str> = input.split('\n').collect();
    // `split` yields a trailing empty element for a newline-terminated input.
    if ends_with_newline {
        lines.pop();
    }
    let last_index = lines.len().saturating_sub(1);

    for (index, line) in lines.iter().enumerate() {
        if records.len() >= MAX_JOURNAL_RECORDS {
            report.bounded = true;
            break;
        }
        if report.malformed >= MAX_MALFORMED_RECORDS {
            report.abandoned_on_malformed = true;
            break;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() > MAX_RECORD_BYTES {
            report.oversized += 1;
            continue;
        }
        match serde_json::from_str::<T>(trimmed) {
            Ok(record) => {
                records.push(record);
                report.accepted += 1;
            }
            Err(_) => {
                // The final line of a file that does not end in a newline was
                // being appended when the process died.
                if index == last_index && !ends_with_newline {
                    report.truncated_tail = true;
                } else {
                    report.malformed += 1;
                }
            }
        }
    }

    Scan { records, report }
}

/// A bounded append-only event counter.
///
/// Event and audit growth is bounded by refusing to append past a ceiling
/// rather than by trimming, so nothing already recorded is silently discarded.
#[derive(Clone, Debug)]
pub struct BoundedEventLog {
    max_events: usize,
    max_bytes: usize,
    events: usize,
    bytes: usize,
}

/// Why an append was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendRefusal {
    EventCeiling,
    ByteCeiling,
    RecordTooLarge,
}

impl BoundedEventLog {
    pub fn new(max_events: usize, max_bytes: usize) -> Self {
        Self {
            max_events,
            max_bytes,
            events: 0,
            bytes: 0,
        }
    }

    pub fn events(&self) -> usize {
        self.events
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Try to append. Refuses at the ceiling instead of growing without bound.
    pub fn append(&mut self, record_bytes: usize) -> Result<(), AppendRefusal> {
        if record_bytes > MAX_RECORD_BYTES {
            return Err(AppendRefusal::RecordTooLarge);
        }
        if self.events >= self.max_events {
            return Err(AppendRefusal::EventCeiling);
        }
        if self.bytes.saturating_add(record_bytes) > self.max_bytes {
            return Err(AppendRefusal::ByteCeiling);
        }
        self.events += 1;
        self.bytes = self.bytes.saturating_add(record_bytes);
        Ok(())
    }

    pub fn remaining_events(&self) -> usize {
        self.max_events.saturating_sub(self.events)
    }
}
