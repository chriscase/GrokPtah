//! Typed, append-only audit write-ahead log.
//!
//! Two ordering rules give the log its meaning:
//!
//! 1. **Intent is written and fsynced before a permit exists.** If the intent
//!    record cannot be made durable, [`crate::HostAuthority::begin_send`]
//!    returns [`crate::AuthorityError::Durability`] and never hands back a
//!    permit, so the dispatch that the record would have described cannot
//!    happen. Pre-effect persistence failure prevents dispatch.
//! 2. **Outcome is written after dispatch.** By then a physical effect is
//!    already possible, so a write failure cannot be reported as an ordinary
//!    failure. It settles
//!    [`crate::UncertainReason::AuditNotDurableAfterDispatch`] instead.
//!
//! Records are content-free: they carry digests and opaque handles, never
//! bodies, secrets, or filesystem paths.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AuthorityError;

/// What an audit record describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuditEvent {
    /// A principal was authenticated.
    Authenticated { principal: String },
    /// The host issued a resource incarnation.
    ResourceIssued {
        resource: String,
        principal: String,
        session: String,
        workspace: String,
    },
    /// A capability was sealed for a principal.
    CapabilitySealed {
        capability: String,
        principal: String,
        effect: String,
    },
    /// A one-use effect lease was minted against an action digest.
    LeaseMinted {
        lease: String,
        capability: String,
        action_digest: String,
        observation_revision: u64,
    },
    /// Intent to perform a physical send. Written *before* the permit exists.
    SendIntent {
        attempt: String,
        lease: String,
        request_digest: String,
        body_digest: String,
    },
    /// How a physical send ended. Written *after* dispatch was possible.
    SendOutcome {
        attempt: String,
        outcome: String,
        detail: String,
    },
    /// An ambiguous attempt was reconciled by an explicit host decision.
    AttemptReconciled { attempt: String, truth: String },
    /// Authority was refused.
    Denied { principal: String, reason: String },
}

/// One durable audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    /// Monotonic sequence number within the log.
    pub sequence: u64,
    /// Milliseconds since the Unix epoch.
    pub recorded_at_ms: u64,
    /// Control epoch in force when the record was written.
    pub control_epoch: u64,
    /// Digest chaining this record to its predecessor, making truncation and
    /// reordering detectable.
    pub previous_digest: String,
    pub event: AuditEvent,
}

/// Append-only audit log over a single file.
#[derive(Debug)]
pub(crate) struct AuditLog {
    path: PathBuf,
    sequence: u64,
    previous_digest: String,
}

const GENESIS_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";

impl AuditLog {
    pub(crate) fn open(root: &Path) -> Result<Self, AuthorityError> {
        let path = root.join("audit.log");
        let mut log = Self {
            path,
            sequence: 0,
            previous_digest: GENESIS_DIGEST.to_string(),
        };
        log.replay()?;
        Ok(log)
    }

    /// Rebuild the chain head from what is on disk.
    ///
    /// A trailing partial line — the signature of a crash mid-append — is
    /// tolerated for *reading* but never rewritten in place; the next append
    /// starts a fresh line, so a torn record stays visible as evidence.
    fn replay(&mut self) -> Result<(), AuthorityError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(AuthorityError::Durability(e.to_string())),
        };
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<AuditRecord>(line) else {
                // Torn or unparsable trailing record: stop replaying here and
                // keep the chain head from the last good record.
                break;
            };
            self.sequence = record.sequence;
            self.previous_digest = record_digest(line);
        }
        Ok(())
    }

    /// Append one record and fsync it.
    ///
    /// Returns only after the bytes are durable, so a caller that sees `Ok`
    /// may rely on the record surviving a crash.
    pub(crate) fn append(
        &mut self,
        control_epoch: u64,
        event: AuditEvent,
    ) -> Result<AuditRecord, AuthorityError> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or_else(|| AuthorityError::Durability("audit sequence exhausted".into()))?;
        let record = AuditRecord {
            sequence,
            recorded_at_ms: crate::unix_time_millis(),
            control_epoch,
            previous_digest: self.previous_digest.clone(),
            event,
        };
        let line = serde_json::to_string(&record)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;

        let mut file: File = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        file.write_all(line.as_bytes())
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        file.write_all(b"\n")
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;
        file.sync_all()
            .map_err(|e| AuthorityError::Durability(e.to_string()))?;

        self.sequence = sequence;
        self.previous_digest = record_digest(&line);
        Ok(record)
    }

    /// Read every well-formed record, oldest first.
    pub(crate) fn records(&self) -> Result<Vec<AuditRecord>, AuthorityError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(AuthorityError::Durability(e.to_string())),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<AuditRecord>(line) {
                Ok(record) => out.push(record),
                Err(_) => break,
            }
        }
        Ok(out)
    }

    /// Verify the hash chain over the persisted records.
    pub(crate) fn verify_chain(&self) -> Result<bool, AuthorityError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(AuthorityError::Durability(e.to_string())),
        };
        let mut expected_prev = GENESIS_DIGEST.to_string();
        let mut expected_seq = 1u64;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(record) = serde_json::from_str::<AuditRecord>(line) else {
                break;
            };
            if record.previous_digest != expected_prev || record.sequence != expected_seq {
                return Ok(false);
            }
            expected_prev = record_digest(line);
            expected_seq = expected_seq.saturating_add(1);
        }
        Ok(true)
    }
}

fn record_digest(line: &str) -> String {
    crate::digest::ContentDigest::of_bytes(line.as_bytes()).to_hex()
}
