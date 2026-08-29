//! A durable record of physical provider-send attempts.
//!
//! **This records; it never authorizes.** There is no permit type, nothing here
//! admits or refuses a send, and no call path is conditioned on it. It is the
//! durable counterpart of the in-memory observation sink `main` already has:
//! write-only in normal operation, read once on restart.
//!
//! That distinction is the whole design. An earlier revision of this branch
//! shipped a `SendLedger` that *gated* dispatch — it minted a `SendPermit`,
//! decided admission, and settled attempts from caller-supplied state. That was
//! a second **authority**, and an exact-head audit rejected it: the permit was
//! not bound to the ledger that minted it, `settle` took caller-supplied audit
//! state, and `resolve_uncertain(.., granted: bool, ..)` was a caller assertion
//! wearing a grant's name. None of those defects can exist here, because
//! nothing in this module decides anything.
//!
//! # What it buys
//!
//! After a crash, the operator can tell a provider call that provably never
//! left from one that may already have been delivered. `main` cannot: its
//! observation sink is in memory, so a crash erases the distinction entirely
//! and every interrupted attempt looks alike.
//!
//! # Ordering, which is the only guarantee that matters
//!
//! [`AttemptState::Preparing`] is fsynced *before* the request is dispatched,
//! and [`AttemptState::Sending`] is fsynced *before* the send future exists. A
//! record found at `Preparing` therefore proves no request byte moved; one at
//! `Sending` proves nothing either way, and says so.
//!
//! # Relationship to #497
//!
//! #497's G3 holds the *authoritative* attempt lattice, bound to a canonical
//! principal, capability and audit chain. This carries none of that binding and
//! does not pretend to: it records session, run, route and request digest,
//! which the bridge already knows, and nothing about authority. When G3 reaches
//! the live send path this file should be **deleted**, not reconciled — its
//! records are operational breadcrumbs, not evidence.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Maximum attempt records retained in one log before it is rotated away.
pub(crate) const MAX_ATTEMPT_RECORDS: usize = 4_096;
/// Maximum bytes for one record line. Longer lines are refused, not truncated.
pub(crate) const MAX_RECORD_BYTES: usize = 8 * 1024;

/// Durable state of one physical send attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AttemptState {
    /// Durable intent exists; the request has not been dispatched.
    Preparing,
    /// Durable immediately before the send future is created.
    Sending,
    /// Evidence proves no request byte reached the provider.
    NotSent,
    /// The provider answered. The exchange has an outcome.
    Settled,
    /// A write may have happened and the outcome is unknown.
    Uncertain,
}

impl AttemptState {
    /// What this state proves about delivery.
    pub(crate) fn delivery_knowledge(self) -> super::DeliveryKnowledge {
        use super::DeliveryKnowledge;
        match self {
            // `Sending` is durable before the send future exists, so a record
            // still at `Preparing` proves the request never left the host.
            Self::Preparing | Self::NotSent => DeliveryKnowledge::KnownNotDelivered,
            Self::Settled => DeliveryKnowledge::KnownDelivered,
            Self::Sending | Self::Uncertain => DeliveryKnowledge::Unknown,
        }
    }

    /// Whether no further record for this ordinal is expected.
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::NotSent | Self::Settled | Self::Uncertain)
    }
}

/// One appended record. Deliberately carries no prompt, body, credential,
/// header or raw URL — only what an operator needs to reason about recovery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AttemptRecord {
    pub(crate) ordinal: u64,
    pub(crate) state: AttemptState,
    /// Public model identifier. Not a route, not a credential.
    ///
    /// The session is deliberately absent: the send helper does not receive one,
    /// and inventing a placeholder would put a value in a durable record that
    /// nothing could rely on.
    pub(crate) model: String,
    /// Digest of the request, so a replayed attempt is recognisable.
    pub(crate) request_digest: String,
    pub(crate) at_ms: u64,
}

/// How interrupted attempts classify after a restart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AttemptRecovery {
    /// Ordinals recorded at `Preparing`: provably nothing was sent.
    pub(crate) provably_not_sent: Vec<u64>,
    /// Ordinals recorded at `Sending`: may have been delivered. Never
    /// auto-retried.
    pub(crate) indeterminate: Vec<u64>,
    /// Ordinals that reached a terminal state before the interruption.
    pub(crate) settled: usize,
    /// Lines that could not be parsed, counted rather than skipped in silence.
    pub(crate) malformed: usize,
    /// A final line with no newline: a crash during an append, not corruption.
    pub(crate) truncated_tail: bool,
}

impl AttemptRecovery {
    pub(crate) fn has_indeterminate(&self) -> bool {
        !self.indeterminate.is_empty()
    }

    /// Operator-facing summary. Host-authored text only.
    pub(crate) fn operator_summary(&self) -> String {
        let mut parts = vec![format!(
            "{} {}",
            self.settled,
            AttemptState::Settled.delivery_knowledge().as_str()
        )];
        if !self.provably_not_sent.is_empty() {
            parts.push(format!("{} never sent", self.provably_not_sent.len()));
        }
        if !self.indeterminate.is_empty() {
            parts.push(format!(
                "{} may have reached the provider",
                self.indeterminate.len()
            ));
        }
        if self.malformed > 0 {
            parts.push(format!("{} malformed", self.malformed));
        }
        if self.truncated_tail {
            parts.push("truncated tail (crash during append)".to_string());
        }
        parts.join(", ")
    }
}

/// Append-only recorder for one home.
#[derive(Debug)]
pub(crate) struct AttemptRecorder {
    path: PathBuf,
    next_ordinal: u64,
    written: usize,
}

impl AttemptRecorder {
    /// Open, reconstructing the next ordinal from whatever survived.
    ///
    /// A damaged log does not stop the recorder: it is breadcrumbs, and
    /// refusing to run because an old line is unreadable would be worse than
    /// the gap it reports. The gap is surfaced through [`Self::recover`].
    pub(crate) fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("provider-attempts.ndjson");
        let scan = Self::scan(&path);
        Ok(Self {
            path,
            // Reconstructed from the maximum ordinal seen, so a crash between
            // allocation and use never reissues one.
            next_ordinal: scan.highest_seen.saturating_add(1),
            written: scan.total_lines,
        })
    }

    /// Record the intent. Returns the ordinal, fsynced before the caller may
    /// dispatch anything.
    pub(crate) fn record_preparing(
        &mut self,
        model: &str,
        request_digest: &str,
        at_ms: u64,
    ) -> std::io::Result<u64> {
        let ordinal = self.next_ordinal;
        self.next_ordinal = ordinal.saturating_add(1);
        self.append(
            &AttemptRecord {
                ordinal,
                state: AttemptState::Preparing,
                model: model.to_string(),
                request_digest: request_digest.to_string(),
                at_ms,
            },
            true,
        )?;
        Ok(ordinal)
    }

    /// Record that the send future is about to exist. Fsynced: after this
    /// returns, the host can no longer prove non-delivery.
    pub(crate) fn record_sending(&mut self, previous: &AttemptRecord) -> std::io::Result<()> {
        self.append(
            &AttemptRecord {
                state: AttemptState::Sending,
                ..previous.clone()
            },
            true,
        )
    }

    /// Record the outcome. Not fsynced: by now the request has either landed or
    /// not, and a lost outcome line recovers as `Sending`, which is the honest
    /// answer rather than an optimistic one.
    pub(crate) fn record_outcome(
        &mut self,
        previous: &AttemptRecord,
        state: AttemptState,
    ) -> std::io::Result<()> {
        debug_assert!(state.is_terminal());
        self.append(
            &AttemptRecord {
                state,
                ..previous.clone()
            },
            false,
        )
    }

    fn append(&mut self, record: &AttemptRecord, sync: bool) -> std::io::Result<()> {
        if self.written >= MAX_ATTEMPT_RECORDS {
            // Bounded by rotation, so growth cannot be unbounded and nothing
            // already recorded is rewritten in place.
            let rotated = self.path.with_extension("ndjson.1");
            let _ = std::fs::rename(&self.path, rotated);
            self.written = 0;
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        if line.len() > MAX_RECORD_BYTES {
            return Err(std::io::Error::other(
                "attempt record exceeds the line bound",
            ));
        }
        line.push('\n');
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())?;
        if sync {
            file.sync_data()?;
        }
        self.written += 1;
        Ok(())
    }

    /// Classify what survived an interruption.
    pub(crate) fn recover(dir: &Path) -> AttemptRecovery {
        let scan = Self::scan(&dir.join("provider-attempts.ndjson"));
        scan.into_report()
    }

    fn scan(path: &Path) -> Scan {
        let mut scan = Scan::default();
        let Ok(file) = File::open(path) else {
            return scan;
        };
        let mut latest: std::collections::BTreeMap<u64, AttemptState> = Default::default();
        let mut last_line_complete = true;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                // An unterminated final line surfaces here as a read that still
                // yields content; a hard error means the tail was cut.
                last_line_complete = false;
                break;
            };
            scan.total_lines += 1;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<AttemptRecord>(&line) {
                Ok(record) => {
                    scan.highest_seen = scan.highest_seen.max(record.ordinal);
                    latest.insert(record.ordinal, record.state);
                }
                Err(_) => scan.malformed += 1,
            }
        }
        // A file not ending in a newline was being appended when the process
        // died. That is a crash cut, not corruption, and the partial line has
        // already been counted as malformed by the parse above; move it.
        if let Ok(bytes) = std::fs::read(path) {
            if !bytes.is_empty() && bytes.last() != Some(&b'\n') && scan.malformed > 0 {
                scan.malformed -= 1;
                scan.truncated_tail = true;
            }
        }
        let _ = last_line_complete;
        scan.latest = latest;
        scan
    }
}

#[derive(Default)]
struct Scan {
    latest: std::collections::BTreeMap<u64, AttemptState>,
    highest_seen: u64,
    total_lines: usize,
    malformed: usize,
    truncated_tail: bool,
}

impl Scan {
    fn into_report(self) -> AttemptRecovery {
        let mut report = AttemptRecovery {
            malformed: self.malformed,
            truncated_tail: self.truncated_tail,
            ..Default::default()
        };
        for (ordinal, state) in self.latest {
            match state {
                AttemptState::Preparing => report.provably_not_sent.push(ordinal),
                AttemptState::Sending => report.indeterminate.push(ordinal),
                _ => report.settled += 1,
            }
        }
        report.provably_not_sent.sort_unstable();
        report.indeterminate.sort_unstable();
        report
    }
}

/// Short, non-invertible handle for the request this attempt carried.
///
/// Over the endpoint and model only — never the body, which would make the
/// record a confirmation oracle for prompt content.
pub(crate) fn request_handle(url: &str, model: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah.durable.attempt.v1\n");
    hasher.update((url.len() as u64).to_be_bytes());
    hasher.update(url.as_bytes());
    hasher.update(model.as_bytes());
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Milliseconds since the Unix epoch, saturating rather than panicking on a
/// clock before it.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    fn seed(path: &Path, lines: &[&str], trailing_newline: bool) {
        let mut body = lines.join("\n");
        if trailing_newline && !body.is_empty() {
            body.push('\n');
        }
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("seed");
    }

    fn record(ordinal: u64, state: &str) -> String {
        format!(
            r#"{{"ordinal":{ordinal},"state":"{state}","model":"m","requestDigest":"d","atMs":1}}"#
        )
    }

    /// The ordering guarantee: `Preparing` proves nothing was sent, `Sending`
    /// proves nothing either way.
    #[test]
    fn preparing_proves_not_sent_and_sending_proves_nothing() {
        assert_eq!(
            AttemptState::Preparing.delivery_knowledge(),
            crate::durable::DeliveryKnowledge::KnownNotDelivered
        );
        assert_eq!(
            AttemptState::Sending.delivery_knowledge(),
            crate::durable::DeliveryKnowledge::Unknown
        );
        assert_eq!(
            AttemptState::Settled.delivery_knowledge(),
            crate::durable::DeliveryKnowledge::KnownDelivered
        );
        assert!(!AttemptState::Sending.delivery_knowledge().may_auto_retry());
        assert!(AttemptState::NotSent.delivery_knowledge().may_auto_retry());
    }

    /// A crash cut mid-turn: recovery separates what provably never left from
    /// what may already have landed. `main` cannot make this distinction at all
    /// after a restart, because its observation sink is in memory.
    #[test]
    fn recovery_separates_never_sent_from_may_have_landed() {
        let home = dir();
        let path = home.path().join("provider-attempts.ndjson");
        seed(
            &path,
            &[
                &record(1, "preparing"),
                &record(1, "sending"),
                &record(1, "settled"),
                &record(2, "preparing"),
                &record(2, "sending"),
                &record(3, "preparing"),
            ],
            true,
        );

        let report = AttemptRecorder::recover(home.path());
        assert_eq!(report.settled, 1, "attempt 1 completed");
        assert_eq!(report.indeterminate, vec![2], "attempt 2 was in flight");
        assert_eq!(
            report.provably_not_sent,
            vec![3],
            "attempt 3 never dispatched"
        );
        assert!(report.has_indeterminate());
        assert!(report
            .operator_summary()
            .contains("may have reached the provider"));
    }

    /// A crash during an append leaves a final line with no newline. That is a
    /// different fault from corruption in the middle and is reported as such.
    #[test]
    fn a_crash_cut_tail_is_not_reported_as_corruption() {
        let home = dir();
        let path = home.path().join("provider-attempts.ndjson");
        seed(
            &path,
            &[&record(1, "preparing"), r#"{"ordinal":2,"state":"prep"#],
            false,
        );
        let report = AttemptRecorder::recover(home.path());
        assert!(report.truncated_tail, "an unterminated tail is a crash cut");
        assert_eq!(report.malformed, 0, "a crash cut is not corruption");
        assert_eq!(report.provably_not_sent, vec![1]);
    }

    /// Repeated malformed records are counted, not skipped in silence.
    #[test]
    fn repeated_malformed_records_are_counted() {
        let home = dir();
        let path = home.path().join("provider-attempts.ndjson");
        let mut lines: Vec<String> = (0..32).map(|_| "{not json".to_string()).collect();
        lines.push(record(7, "sending"));
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        seed(&path, &refs, true);

        let report = AttemptRecorder::recover(home.path());
        assert_eq!(report.malformed, 32);
        assert_eq!(
            report.indeterminate,
            vec![7],
            "the readable record still counts"
        );
        assert!(report.operator_summary().contains("32 malformed"));
    }

    /// Restart reconstructs the maximum ordinal, so a crash between allocation
    /// and use never reissues one.
    #[test]
    fn restart_reconstructs_the_maximum_ordinal() {
        let home = dir();
        let mut recorder = AttemptRecorder::open(home.path()).expect("open");
        assert_eq!(recorder.record_preparing("m", "d", 1).expect("first"), 1);
        assert_eq!(recorder.record_preparing("m", "d", 2).expect("second"), 2);
        drop(recorder);

        let mut reopened = AttemptRecorder::open(home.path()).expect("reopen");
        assert_eq!(
            reopened
                .record_preparing("m", "d", 3)
                .expect("after restart"),
            3,
            "the next ordinal follows the maximum seen"
        );
    }

    /// Growth is bounded by rotation, so a long-lived home cannot accumulate an
    /// unbounded log.
    #[test]
    fn growth_is_bounded_by_rotation() {
        let home = dir();
        let mut recorder = AttemptRecorder::open(home.path()).expect("open");
        for index in 0..(MAX_ATTEMPT_RECORDS + 8) {
            recorder
                .record_preparing("m", "d", index as u64)
                .expect("append");
        }
        let live = std::fs::read_to_string(home.path().join("provider-attempts.ndjson"))
            .expect("live log");
        assert!(
            live.lines().count() <= MAX_ATTEMPT_RECORDS,
            "the live log stays bounded"
        );
        assert!(
            home.path().join("provider-attempts.ndjson.1").exists(),
            "the previous segment is rotated, not deleted in place"
        );
    }

    /// The record carries no prompt, body, credential or raw URL.
    #[test]
    fn the_record_carries_no_request_content() {
        let handle = request_handle("https://example.invalid/v1/chat/completions", "model-x");
        assert_eq!(handle.len(), 16, "a short, non-invertible handle");
        assert!(!handle.contains("example"));
        assert_ne!(
            handle,
            request_handle("https://other.invalid/v1/chat/completions", "model-x"),
            "different endpoints are distinguishable"
        );

        let encoded = serde_json::to_string(&AttemptRecord {
            ordinal: 1,
            state: AttemptState::Sending,
            model: "model-x".into(),
            request_digest: handle,
            at_ms: 1,
        })
        .expect("serializes");
        for forbidden in ["prompt", "authorization", "bearer", "https://"] {
            assert!(!encoded.to_ascii_lowercase().contains(forbidden));
        }
    }

    /// An absent log is not an error: the recorder is breadcrumbs, and a fresh
    /// home simply has none.
    #[test]
    fn an_absent_log_recovers_as_empty() {
        let home = dir();
        let report = AttemptRecorder::recover(home.path());
        assert_eq!(report, AttemptRecovery::default());
        assert!(!report.has_indeterminate());
    }
}
