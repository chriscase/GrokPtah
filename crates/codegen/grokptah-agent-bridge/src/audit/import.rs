//! Legacy v1 audit import (#443).
//!
//! The v1 ledger (`orchestration/store.rs`) wrote `audit/audit.jsonl` and
//! rotated it onto `audit.jsonl.1`, deleting whatever was there before. Those
//! bytes carry no sequence numbers and no chain, so they cannot be
//! retroactively authenticated. Import therefore preserves them **verbatim**
//! and labels them:
//!
//! - `originAuthenticated: false` — the bytes are preserved, not vouched for;
//! - `sequenceOrigin: import_assigned` — the sequences were assigned here, not
//!   issued by any ledger;
//! - `precedingLossUnknown: true` on the oldest imported generation, because
//!   v1 already destroyed everything older than `audit.jsonl.1` without a
//!   record.
//!
//! The *boundary* is authenticated: each imported generation's `finalTag` is
//! an HMAC over its exact SHA-256, so the first native generation still chains
//! from a real tag and continuity holds across the import.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::canon::canonical_bytes_without_mac;
use super::files;
use super::keys::{sha256_hex, AuditKeys};
use super::{AuditError, AuditResult, PoisonReason};

pub(crate) const BOOTSTRAP_SCHEMA: &str = "grokptah-audit-bootstrap.v2";

/// Declares the generation directories a first-open import is about to create,
/// so a crash before the manifest commit is recoverable without guessing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BootstrapMarker {
    pub schema: String,
    pub generation_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub mac: String,
}

impl BootstrapMarker {
    pub(crate) fn new(generation_ids: Vec<String>, keys: &AuditKeys) -> AuditResult<Self> {
        let mut marker = Self {
            schema: BOOTSTRAP_SCHEMA.to_string(),
            generation_ids,
            created_at: Utc::now(),
            mac: String::new(),
        };
        let payload = canonical_bytes_without_mac(&marker)?;
        marker.mac = keys.manifest_mac(&payload);
        Ok(marker)
    }

    pub(crate) fn verify(&self, keys: &AuditKeys) -> AuditResult<()> {
        if self.schema != BOOTSTRAP_SCHEMA {
            return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
        }
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.manifest_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Poisoned(PoisonReason::ManifestMacMismatch));
        }
        Ok(())
    }
}

pub(crate) fn bootstrap_path(root: &Path) -> PathBuf {
    root.join("bootstrap.json")
}

#[derive(Debug, Clone)]
pub(crate) struct LegacyGenerationPlan {
    pub source_name: String,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub lines: u64,
    pub preceding_loss_unknown: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct LegacyImportPlan {
    pub generations: Vec<LegacyGenerationPlan>,
}

/// Read the v1 files, oldest first. `audit.jsonl.1` predates `audit.jsonl`.
pub(crate) fn plan_legacy_import(legacy_dir: &Path) -> AuditResult<LegacyImportPlan> {
    let mut plan = LegacyImportPlan::default();
    for name in ["audit.jsonl.1", "audit.jsonl"] {
        let path = legacy_dir.join(name);
        if !path.exists() {
            continue;
        }
        files::reject_symlink(&path)?;
        let bytes = files::read_bytes(&path)?;
        if bytes.is_empty() {
            continue;
        }
        let sha256 = sha256_hex(&bytes);
        let lines = count_legacy_lines(&bytes);
        plan.generations.push(LegacyGenerationPlan {
            source_name: name.to_string(),
            bytes,
            sha256,
            lines,
            preceding_loss_unknown: false,
        });
    }
    if let Some(first) = plan.generations.first_mut() {
        // v1 deleted everything older than the file we are importing first,
        // and left no record that it existed. Carry that forward as a fact.
        first.preceding_loss_unknown = true;
    }
    Ok(plan)
}

pub(crate) fn count_legacy_lines(bytes: &[u8]) -> u64 {
    if bytes.is_empty() {
        return 0;
    }
    bytes.iter().filter(|byte| **byte == b'\n').count() as u64
        + u64::from(bytes.last() != Some(&b'\n'))
}

/// Write imported bytes verbatim into a generation journal.
pub(crate) fn write_imported_journal(path: &Path, bytes: &[u8]) -> AuditResult<()> {
    use std::io::Write;

    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| AuditError::Io(format!("replace empty journal: {error}")))?;
    }
    let mut file = files::create_private_file_new(path)?;
    file.write_all(bytes)
        .map_err(|error| AuditError::Io(format!("write imported journal: {error}")))?;
    file.sync_all()
        .map_err(|error| AuditError::Io(format!("sync imported journal: {error}")))?;
    Ok(())
}
