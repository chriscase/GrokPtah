//! Sealed audit export and independent verification (#443).
//!
//! A v1 verifier rejects unknown files, so a v2 export cannot be a v1 export
//! with extra documents added. The selector below therefore refuses to emit v1
//! for anything a v1 document cannot honestly represent, and `auto` never
//! produces a misleading answer in either direction.
//!
//! Export never rotates, never truncates, never deletes, and never advances the
//! manifest epoch. Producing an export is not permission to delete anything.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::canon::canonical_bytes_without_mac;
use super::documents::*;
use super::files;
use super::keys::{sha256_hex, AuditKeys};
use super::ledger::{scan_journal_at, AuditLedger};
use super::witness::WitnessState;
use super::{AuditError, AuditResult, PoisonReason, RefuseReason};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// v1 when the ledger has never rotated and holds nothing unauthenticated;
    /// v2 otherwise. Never emits a document that misrepresents the range.
    Auto,
    V1,
    V2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageKind {
    Generation,
    /// A range removed by an authorized retention transaction. The chain is
    /// still stitched across it by `chainBase`/`finalTag`.
    Hole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageElement {
    pub kind: CoverageKind,
    pub generation_id: String,
    pub first_seq: u64,
    pub last_seq: u64,
    pub chain_base: String,
    pub final_tag: String,
    pub journal_sha256: String,
    pub journal_bytes: u64,
    pub entry_count: u64,
    /// `false` for imported legacy bytes: preserved, never vouched for.
    pub origin_authenticated: bool,
    pub preceding_loss_unknown: bool,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub retention_epoch: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub export_seal_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportManifest {
    pub schema: String,
    pub seal_id: String,
    pub installation_id: String,
    pub key_id: String,
    pub manifest_epoch: u64,
    pub retention_epoch: u64,
    pub global_first_seq: u64,
    pub global_last_seq: u64,
    pub witness_state: WitnessState,
    /// `true` only when every coverage element is a generation.
    pub complete: bool,
    pub coverage: Vec<CoverageElement>,
    pub exported_at: DateTime<Utc>,
    pub mac: String,
}

impl ExportManifest {
    fn seal(&mut self, keys: &AuditKeys) -> AuditResult<()> {
        self.mac = String::new();
        let payload = canonical_bytes_without_mac(&*self)?;
        self.mac = keys.seal_mac(&payload);
        Ok(())
    }

    fn verify_mac(&self, keys: &AuditKeys) -> AuditResult<()> {
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.seal_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
        }
        Ok(())
    }
}

/// Path-free operator receipt.
#[derive(Debug, Clone)]
pub struct ExportReceipt {
    pub seal_id: String,
    pub schema: String,
    pub complete: bool,
    pub generations_exported: usize,
    pub holes: usize,
    pub global_first_seq: u64,
    pub global_last_seq: u64,
    pub unauthenticated_generations: usize,
    pub witness_state: WitnessState,
}

#[derive(Debug, Clone)]
pub struct ExportVerification {
    pub seal_id: String,
    pub schema: String,
    pub complete: bool,
    pub generations_verified: usize,
    pub holes: usize,
    pub global_first_seq: u64,
    pub global_last_seq: u64,
    pub unauthenticated_generations: usize,
    pub witness_state: WitnessState,
}

impl AuditLedger {
    pub fn export(&self, dest: &Path, format: ExportFormat) -> AuditResult<ExportReceipt> {
        let _transaction = self.operation_lock.lock();
        if let Some(poison) = self.is_poisoned() {
            return Err(AuditError::Poisoned(poison));
        }
        if self.open_intents() != 0 {
            return Err(AuditError::Refused(RefuseReason::OpenIntentsPresent));
        }
        if dest.exists() {
            return Err(AuditError::Refused(RefuseReason::ExportDestinationExists));
        }
        if let Some(parent) = dest.parent() {
            files::reject_symlink_components(parent)?;
        }

        let manifest = self.manifest_snapshot();
        let multi = manifest.generations.len() > 1 || !manifest.tombstones.is_empty();
        let unauthenticated = manifest
            .generations
            .iter()
            .filter(|g| !g.origin_authenticated)
            .count();
        // A v1 document has no way to say "partial" and no way to say
        // "unauthenticated origin", so emitting one for either state would
        // misrepresent the range. Refusing is mandatory, not a convenience.
        let v1_possible = !multi && unauthenticated == 0;
        let schema = match format {
            ExportFormat::V1 if !v1_possible => {
                return Err(AuditError::Refused(
                    RefuseReason::ExportV1IncompatibleMultiGeneration,
                ))
            }
            ExportFormat::V1 => EXPORT_SCHEMA_V1,
            ExportFormat::Auto if v1_possible => EXPORT_SCHEMA_V1,
            _ => EXPORT_SCHEMA_V2,
        };

        // Coverage must tile globalFirstSeq..globalLastSeq exactly.
        let mut coverage: Vec<CoverageElement> = Vec::new();
        let mut expected_seq = manifest.global_first_seq;
        let mut global_last_seq = manifest.global_first_seq.saturating_sub(1);
        for descriptor in &manifest.generations {
            if descriptor.first_seq != expected_seq {
                return Err(AuditError::Poisoned(PoisonReason::SequenceDiscontinuity));
            }
            let element = if descriptor.state == GenerationState::Tombstoned {
                let tombstone = manifest
                    .tombstones
                    .iter()
                    .find(|t| t.generation_id == descriptor.generation_id)
                    .ok_or(AuditError::Poisoned(PoisonReason::TombstoneInconsistent))?;
                CoverageElement {
                    kind: CoverageKind::Hole,
                    generation_id: tombstone.generation_id.clone(),
                    first_seq: tombstone.first_seq,
                    last_seq: tombstone.last_seq,
                    chain_base: tombstone.chain_base.clone(),
                    final_tag: tombstone.final_tag.clone(),
                    journal_sha256: tombstone.journal_sha256.clone(),
                    journal_bytes: tombstone.journal_bytes,
                    entry_count: tombstone.entry_count,
                    origin_authenticated: descriptor.origin_authenticated,
                    preceding_loss_unknown: descriptor.preceding_loss_unknown,
                    retention_epoch: Some(tombstone.retention_epoch),
                    export_seal_id: tombstone.export_seal_id.clone(),
                }
            } else {
                let verification = self.verify_generation(&descriptor.generation_id)?;
                CoverageElement {
                    kind: CoverageKind::Generation,
                    generation_id: descriptor.generation_id.clone(),
                    first_seq: descriptor.first_seq,
                    last_seq: verification.last_seq,
                    chain_base: descriptor.chain_base.clone(),
                    final_tag: verification.final_tag.clone(),
                    journal_sha256: verification.journal_sha256.clone(),
                    journal_bytes: verification.journal_bytes,
                    entry_count: verification.entry_count,
                    origin_authenticated: descriptor.origin_authenticated,
                    preceding_loss_unknown: descriptor.preceding_loss_unknown,
                    retention_epoch: None,
                    export_seal_id: None,
                }
            };
            expected_seq = element.last_seq.saturating_add(1);
            global_last_seq = element.last_seq;
            coverage.push(element);
        }

        files::create_private_dir_new(dest)?;
        // Everything after the destination is created runs inside one fallible
        // scope: a failed export must never leave a half-written directory that
        // could be mistaken for a sealed one.
        let sealed = (|| -> AuditResult<(ExportManifest, ExportVerification)> {
            for element in &coverage {
                if element.kind == CoverageKind::Hole {
                    continue;
                }
                let source = Self::generation_dir(self.root(), &element.generation_id);
                let target = if schema == EXPORT_SCHEMA_V1 {
                    dest.to_path_buf()
                } else {
                    let target = dest.join("generations").join(&element.generation_id);
                    files::create_private_dir_all(&target)?;
                    target
                };
                for name in ["journal.jsonl", "anchor.json"] {
                    let bytes = files::read_bytes(&source.join(name))?;
                    files::atomic_write(&target.join(name), &bytes)?;
                }
            }
            if schema == EXPORT_SCHEMA_V2 {
                let bytes = files::read_bytes(&Self::manifest_path(self.root()))?;
                files::atomic_write(&dest.join("manifest.json"), &bytes)?;
            }

            let complete = coverage.iter().all(|c| c.kind == CoverageKind::Generation);
            let seal_id = format!(
                "seal-{}",
                &self.keys().opaque_digest(&format!("{}", Uuid::new_v4()))[..32]
            );
            let mut export = ExportManifest {
                schema: schema.to_string(),
                seal_id: seal_id.clone(),
                installation_id: manifest.installation_id.clone(),
                key_id: manifest.key_id.clone(),
                manifest_epoch: manifest.manifest_epoch,
                retention_epoch: manifest.retention_epoch,
                global_first_seq: manifest.global_first_seq,
                global_last_seq,
                witness_state: self.witness_state(),
                complete,
                coverage,
                exported_at: Utc::now(),
                mac: String::new(),
            };
            export.seal(&self.keys())?;
            let bytes = serde_json::to_vec(&export)
                .map_err(|error| AuditError::Io(format!("serialize export manifest: {error}")))?;
            files::atomic_write(&dest.join("export-manifest.json"), &bytes)?;
            files::fsync_dir(dest)?;

            // Independent re-verification: reopen the copy with a fresh reader
            // that shares no state with the live ledger, and only then let a
            // receipt be returned.
            let verification = verify_export(dest, &self.keys())?;
            if verification.seal_id != seal_id || verification.complete != complete {
                return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
            }
            self.record_export_seal(&export)?;
            Ok((export, verification))
        })();

        let (export, verification) = match sealed {
            Ok(sealed) => sealed,
            Err(error) => {
                // Only a destination this call created is ever removed.
                let _ = std::fs::remove_dir_all(dest);
                return Err(error);
            }
        };

        Ok(ExportReceipt {
            seal_id: export.seal_id,
            schema: export.schema,
            complete: export.complete,
            generations_exported: verification.generations_verified,
            holes: verification.holes,
            global_first_seq: export.global_first_seq,
            global_last_seq: export.global_last_seq,
            unauthenticated_generations: verification.unauthenticated_generations,
            witness_state: export.witness_state,
        })
    }
}

const EXPORT_SEALS_FILE: &str = "export-seals.json";

impl AuditLedger {
    fn export_seals_path(&self) -> std::path::PathBuf {
        self.root().join(EXPORT_SEALS_FILE)
    }

    fn record_export_seal(&self, export: &ExportManifest) -> AuditResult<()> {
        let path = self.export_seals_path();
        let mut file = if path.exists() {
            files::reject_symlink(&path)?;
            let bytes = files::read_bytes(&path)?;
            let file = serde_json::from_slice::<ExportSealFile>(&bytes)
                .map_err(|_| AuditError::Poisoned(PoisonReason::ExportMacMismatch))?;
            file.verify(&self.keys())?;
            file
        } else {
            ExportSealFile::new()
        };
        if file.seals.iter().any(|seal| seal.seal_id == export.seal_id) {
            return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
        }
        file.seals.push(ExportSealRecord {
            seal_id: export.seal_id.clone(),
            generation_ids: export
                .coverage
                .iter()
                .filter(|element| element.kind == CoverageKind::Generation)
                .map(|element| element.generation_id.clone())
                .collect(),
            global_first_seq: export.global_first_seq,
            global_last_seq: export.global_last_seq,
            complete: export.complete,
            committed_at: Utc::now(),
        });
        file.updated_at = Utc::now();
        file.seal(&self.keys())?;
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| AuditError::Io(format!("serialize export seals: {error}")))?;
        files::atomic_write(&path, &bytes)
    }

    pub(crate) fn has_valid_export_seal(
        &self,
        seal_id: &str,
        generation_id: &str,
    ) -> AuditResult<bool> {
        let path = self.export_seals_path();
        if !path.exists() {
            return Ok(false);
        }
        files::reject_symlink(&path)?;
        let bytes = files::read_bytes(&path)?;
        let file: ExportSealFile = serde_json::from_slice(&bytes)
            .map_err(|_| AuditError::Poisoned(PoisonReason::ExportMacMismatch))?;
        file.verify(&self.keys())?;
        Ok(file.seals.iter().any(|seal| {
            seal.seal_id == seal_id && seal.generation_ids.iter().any(|id| id == generation_id)
        }))
    }
}

/// Verify a sealed export directory from scratch.
///
/// Accepts both `grokptah-audit-export.v1` and `.v2`, so exports taken before
/// generations existed stay verifiable forever.
pub fn verify_export(dir: &Path, keys: &AuditKeys) -> AuditResult<ExportVerification> {
    files::reject_symlink_components(dir)?;
    files::reject_symlink(dir)?;
    let path = dir.join("export-manifest.json");
    files::reject_symlink(&path)?;
    let bytes = files::read_bytes(&path)?;
    let export: ExportManifest = serde_json::from_slice(&bytes)
        .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
    if export.schema != EXPORT_SCHEMA_V1 && export.schema != EXPORT_SCHEMA_V2 {
        return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
    }
    export.verify_mac(keys)?;

    if export.installation_id != keys.installation_id() {
        return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
    }
    if export.key_id.is_empty() {
        return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
    }
    if export.schema == EXPORT_SCHEMA_V2 {
        let manifest_path = dir.join("manifest.json");
        files::reject_symlink(&manifest_path)?;
        let manifest_bytes = files::read_bytes(&manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
        manifest.verify(keys)?;
        if manifest.installation_id != export.installation_id
            || manifest.key_id != export.key_id
            || manifest.manifest_epoch != export.manifest_epoch
        {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
    }

    if export.schema == EXPORT_SCHEMA_V1 {
        if export.coverage.len() != 1 || !export.complete {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
        if export.coverage[0].kind != CoverageKind::Generation
            || !export.coverage[0].origin_authenticated
        {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
    }

    let mut expected_seq = export.global_first_seq;
    let mut generations_verified = 0usize;
    let mut holes = 0usize;
    let mut unauthenticated = 0usize;
    let mut previous_tag: Option<String> = None;

    for element in &export.coverage {
        if !valid_generation_id(&element.generation_id) || element.generation_id == "g-000000" {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
        if element.first_seq != expected_seq {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
        if let Some(previous) = &previous_tag {
            // The chain must be stitched across holes as well as generations.
            if &element.chain_base != previous {
                return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
            }
        }
        if !element.origin_authenticated {
            unauthenticated += 1;
        }
        match element.kind {
            CoverageKind::Hole => holes += 1,
            CoverageKind::Generation => {
                let generation_dir = if export.schema == EXPORT_SCHEMA_V1 {
                    dir.to_path_buf()
                } else {
                    dir.join("generations").join(&element.generation_id)
                };
                files::reject_symlink(&generation_dir)?;
                let journal = generation_dir.join("journal.jsonl");
                let anchor = generation_dir.join("anchor.json");
                files::reject_symlink(&journal)?;
                files::reject_symlink(&anchor)?;
                let bytes = files::read_bytes(&journal)?;
                if sha256_hex(&bytes) != element.journal_sha256 {
                    return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
                }
                if bytes.len() as u64 != element.journal_bytes {
                    return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
                }
                let scan = scan_journal_at(
                    keys,
                    &journal,
                    &element.generation_id,
                    &element.chain_base,
                    element.first_seq,
                    element.origin_authenticated,
                )?;
                if scan.torn.is_some()
                    || scan.last_seq != element.last_seq
                    || scan.last_tag != element.final_tag
                {
                    return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
                }
                let anchor_bytes = files::read_bytes(&anchor)?;
                let anchor: Anchor = serde_json::from_slice(&anchor_bytes)
                    .map_err(|_| AuditError::Poisoned(PoisonReason::AnchorMacMismatch))?;
                anchor.verify(keys, &element.generation_id)?;
                if anchor.last_seq != element.last_seq
                    || anchor.last_tag != element.final_tag
                    || anchor.journal_bytes != element.journal_bytes
                {
                    return Err(AuditError::Poisoned(PoisonReason::AnchorStateMismatch));
                }
                generations_verified += 1;
            }
        }
        expected_seq = element.last_seq.saturating_add(1);
        previous_tag = Some(element.final_tag.clone());
    }

    if expected_seq.saturating_sub(1) != export.global_last_seq {
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }
    if export.complete != (holes == 0) {
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }

    Ok(ExportVerification {
        seal_id: export.seal_id,
        schema: export.schema,
        complete: export.complete,
        generations_verified,
        holes,
        global_first_seq: export.global_first_seq,
        global_last_seq: export.global_last_seq,
        unauthenticated_generations: unauthenticated,
        witness_state: export.witness_state,
    })
}
