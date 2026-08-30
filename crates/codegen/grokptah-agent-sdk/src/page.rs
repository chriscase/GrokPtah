//! Event cursors and retained-journal range.

use serde::Serialize;
use serde_json::Value;

/// Exclusive durable cursor (`after_seq`) for `ptah_get_events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Cursor {
    after_seq: u64,
}

impl Cursor {
    pub fn from_after_seq(after_seq: u64) -> Self {
        Self { after_seq }
    }

    pub fn after_seq(self) -> u64 {
        self.after_seq
    }
}

/// Host-retained journal window carried on `cursor_expired` (`eventRange`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct RetainedRange {
    pub start_seq: u64,
    pub end_seq: Option<u64>,
}

impl RetainedRange {
    pub(crate) fn from_host(value: Option<&Value>) -> Option<Self> {
        let value = value?;
        let start_seq = value.get("startSeq").and_then(Value::as_u64)?;
        let end_seq = value.get("endSeq").and_then(Value::as_u64);
        Some(Self { start_seq, end_seq })
    }
}
