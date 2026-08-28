//! Sealed audit export and independent verification (#443).
//!
//! A v1 verifier rejects unknown files, so a v2 export cannot be a v1 export
//! with extra documents added. The selector below therefore refuses to emit v1
//! for anything a v1 document cannot honestly represent, and `auto` never
//! produces a misleading answer in either direction.
//!
//! Export never rotates, never truncates, never deletes, and never changes a
//! journal byte, a tombstone or a chain tag. It does commit two additive facts
//! to the manifest: the seal it issued once an independent reader has verified
//! the written copy, and -- for a privileged raw export -- the single-use grant
//! it spent. Without the first, "this range was already exported" would be an
//! unverifiable claim made by whoever wanted the range deleted.
//!
//! Producing an export is still not permission to delete anything: retention
//! matches a seal against the exact range it carried, and a public export that
//! withheld a range records nothing about it at all.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::authority::{AuditCapability, AuthorityGrant, PRIVILEGED_RAW_EXPORT_SUBJECT};
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
    /// A range whose bytes exist but are not carried by this export because
    /// the scope forbids them. Used for imported v1 bytes in a public export:
    /// they were never redacted to the v2 rules and can contain workspace
    /// paths, free-text `detail`, IO strings and provider material.
    Withheld,
}

/// Who an export is for.
///
/// Imported v1 bytes are preserved verbatim by design, which means they still
/// carry whatever the v1 ledger recorded. A public export must therefore not
/// carry them, and a raw preservation export must say plainly that it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportScope {
    /// Redacted: every carried generation was written under the v2 privacy
    /// rules. Unauthenticated legacy ranges are withheld and declared.
    Public,
    /// Privileged raw preservation: carries unauthenticated legacy bytes
    /// verbatim. For operator custody only, never a public artifact.
    PrivilegedRaw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldReason {
    /// Imported v1 bytes: preserved, never redacted, never public.
    UnauthenticatedLegacy,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub withheld_reason: Option<WithheldReason>,
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
    pub scope: ExportScope,
    /// `true` when this export carries verbatim v1 bytes that were never
    /// redacted to the v2 rules. Always `false` for a public export.
    pub contains_unauthenticated_legacy: bool,
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
    pub scope: ExportScope,
    pub contains_unauthenticated_legacy: bool,
    pub withheld: usize,
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
    pub scope: ExportScope,
    pub contains_unauthenticated_legacy: bool,
    pub withheld: usize,
    pub complete: bool,
    pub generations_verified: usize,
    pub holes: usize,
    pub global_first_seq: u64,
    pub global_last_seq: u64,
    pub unauthenticated_generations: usize,
    pub witness_state: WitnessState,
}

impl AuditLedger {
    /// Redacted public export. Needs no capability: it carries nothing that
    /// was not written under the v2 privacy rules.
    pub fn export(&self, dest: &Path, format: ExportFormat) -> AuditResult<ExportReceipt> {
        self.export_scoped(dest, format, ExportScope::Public, None)
    }

    /// Privileged raw preservation export.
    ///
    /// Carries imported v1 bytes verbatim -- workspace paths, free-text
    /// `detail`, IO strings and provider material that were never redacted --
    /// so it requires a verified single-use grant. Naming a different scope is
    /// not authority; the grant is.
    pub fn export_privileged_raw(
        &self,
        dest: &Path,
        format: ExportFormat,
        grant: &AuthorityGrant,
    ) -> AuditResult<ExportReceipt> {
        self.export_scoped(dest, format, ExportScope::PrivilegedRaw, Some(grant))
    }

    fn export_scoped(
        &self,
        dest: &Path,
        format: ExportFormat,
        scope: ExportScope,
        grant: Option<&AuthorityGrant>,
    ) -> AuditResult<ExportReceipt> {
        if dest.exists() {
            return Err(AuditError::Refused(RefuseReason::ExportDestinationExists));
        }

        // Take the barrier *before* the completeness checks. Checking first let
        // an intent open in the check-to-lock window, and the export would then
        // claim a completeness that was no longer true.
        let mut tx = self.structural_tx();
        if let Some(poison) = tx.poisoned() {
            return Err(AuditError::Poisoned(poison));
        }
        if tx.open_intents() != 0 {
            return Err(AuditError::Refused(RefuseReason::OpenIntentsPresent));
        }
        // Spend the grant *before* a single byte is copied. A crash after this
        // leaves the grant spent and no export, which is the safe direction;
        // spending it afterwards would leave a live grant beside a written
        // privileged export.
        if scope == ExportScope::PrivilegedRaw {
            let grant = grant.ok_or(AuditError::Refused(RefuseReason::AuthorityUnavailable))?;
            let mut staged = tx.manifest_clone();
            tx.stage_authority(
                &mut staged,
                grant,
                AuditCapability::PrivilegedRawExport,
                PRIVILEGED_RAW_EXPORT_SUBJECT,
            )?;
            tx.commit_manifest(staged)?;
        }
        let manifest = tx.manifest_clone();
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
        // A public export of imported v1 bytes withholds them, so the range is
        // partial by construction and v1 cannot represent it either way.
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
                    withheld_reason: None,
                }
            } else if scope == ExportScope::Public && !descriptor.origin_authenticated {
                // Preserved, never redacted, never public.
                let verification = tx.verify_generation(&descriptor.generation_id)?;
                CoverageElement {
                    kind: CoverageKind::Withheld,
                    generation_id: descriptor.generation_id.clone(),
                    first_seq: descriptor.first_seq,
                    last_seq: verification.last_seq,
                    chain_base: descriptor.chain_base.clone(),
                    final_tag: verification.final_tag.clone(),
                    journal_sha256: verification.journal_sha256.clone(),
                    journal_bytes: verification.journal_bytes,
                    entry_count: verification.entry_count,
                    origin_authenticated: false,
                    preceding_loss_unknown: descriptor.preceding_loss_unknown,
                    retention_epoch: None,
                    export_seal_id: None,
                    withheld_reason: Some(WithheldReason::UnauthenticatedLegacy),
                }
            } else {
                let verification = tx.verify_generation(&descriptor.generation_id)?;
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
                    withheld_reason: None,
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
        let sealed = (|| -> AuditResult<ExportManifest> {
            for element in &coverage {
                if element.kind != CoverageKind::Generation {
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
            // `Withheld` and `Hole` both make the range partial.
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
                witness_state: tx.witness_state(),
                scope,
                contains_unauthenticated_legacy: coverage
                    .iter()
                    .any(|c| c.kind == CoverageKind::Generation && !c.origin_authenticated),
                complete,
                coverage,
                exported_at: Utc::now(),
                mac: String::new(),
            };
            export.seal(self.keys())?;
            let bytes = serde_json::to_vec(&export)
                .map_err(|error| AuditError::Io(format!("serialize export manifest: {error}")))?;
            files::atomic_write(&dest.join("export-manifest.json"), &bytes)?;
            files::fsync_dir(dest)?;

            Ok(export)
        })();

        let export = match sealed {
            Ok(export) => export,
            Err(error) => {
                // Only a destination this call created is ever removed.
                let _ = std::fs::remove_dir_all(dest);
                return Err(error);
            }
        };
        drop(tx);

        // Independent re-verification: a fresh reader over the copy, holding
        // no ledger state and no barrier.
        let verified = (|| -> AuditResult<ExportVerification> {
            let verification = verify_export(dest, self.keys())?;
            if verification.seal_id != export.seal_id || verification.complete != export.complete {
                return Err(AuditError::Poisoned(PoisonReason::ExportMacMismatch));
            }
            Ok(verification)
        })();
        let verification = match verified {
            Ok(sealed) => sealed,
            Err(error) => {
                // Only a destination this call created is ever removed.
                let _ = std::fs::remove_dir_all(dest);
                return Err(error);
            }
        };

        // The seal enters the registry only now: after the copy was written
        // *and* re-verified from disk by a reader holding no ledger state.
        // Recording it earlier would let a failed export leave behind a seal
        // that authorizes deleting a range nothing preserved. A crash between
        // the verified copy and this commit simply leaves no seal, so the
        // range stays undeletable until it is exported again.
        let registered = self.record_seal(SealRecord {
            seal_id: export.seal_id.clone(),
            schema: export.schema.clone(),
            contains_unauthenticated_legacy: export.contains_unauthenticated_legacy,
            sealed_at: export.exported_at,
            carried: export
                .coverage
                .iter()
                .filter(|element| element.kind == CoverageKind::Generation)
                .map(|element| SealedRange {
                    generation_id: element.generation_id.clone(),
                    first_seq: element.first_seq,
                    last_seq: element.last_seq,
                    final_tag: element.final_tag.clone(),
                    journal_sha256: element.journal_sha256.clone(),
                    entry_count: element.entry_count,
                })
                .collect(),
        });
        if let Err(error) = registered {
            // Same contract as every other failure here: an export that
            // returns an error leaves no destination behind. A copy the ledger
            // cannot vouch for is worse than no copy, because its seal id
            // would look like retention authority it does not carry.
            let _ = std::fs::remove_dir_all(dest);
            return Err(error);
        }

        Ok(ExportReceipt {
            seal_id: export.seal_id,
            schema: export.schema,
            scope: export.scope,
            contains_unauthenticated_legacy: export.contains_unauthenticated_legacy,
            withheld: verification.withheld,
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

/// Verify a sealed export directory from scratch.
///
/// Accepts both `grokptah-audit-export.v1` and `.v2`, so exports taken before
/// generations existed stay verifiable forever.
pub fn verify_export(dir: &Path, keys: &AuditKeys) -> AuditResult<ExportVerification> {
    let path = dir.join("export-manifest.json");
    files::reject_symlink(&path)?;
    let bytes = files::read_bytes(&path)?;
    let export: ExportManifest = serde_json::from_slice(&bytes)
        .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
    if export.schema != EXPORT_SCHEMA_V1 && export.schema != EXPORT_SCHEMA_V2 {
        return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
    }
    export.verify_mac(keys)?;

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

    // The copied ledger manifest is evidence too, and it was going
    // unauthenticated: a v2 export carries `manifest.json`, and nothing here
    // checked its MAC or that it describes the same ledger this seal covers.
    // A swapped-in manifest could have misdescribed generations, tombstones
    // and retention epochs to a reader who trusted the directory as a whole.
    if export.schema == EXPORT_SCHEMA_V2 {
        let path = dir.join("manifest.json");
        files::reject_symlink(&path)?;
        let bytes = files::read_bytes(&path)?;
        let carried: Manifest = serde_json::from_slice(&bytes)
            .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
        carried.verify(keys)?;
        if carried.installation_id != export.installation_id
            || carried.key_id != export.key_id
            || carried.manifest_epoch != export.manifest_epoch
            || carried.retention_epoch != export.retention_epoch
            || carried.global_first_seq != export.global_first_seq
        {
            // Authentic, but describing a different ledger or a different
            // moment than the seal claims.
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
    }

    let mut expected_seq = export.global_first_seq;
    let mut generations_verified = 0usize;
    let mut holes = 0usize;
    let mut withheld = 0usize;
    let mut unauthenticated = 0usize;
    let mut previous_tag: Option<String> = None;

    for element in &export.coverage {
        if element.first_seq != expected_seq {
            return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
        }
        if let Some(previous) = &previous_tag {
            // The chain must be stitched across holes as well as generations.
            if &element.chain_base != previous {
                return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
            }
        }
        // Only a *carried* generation can leak. A withheld element names an
        // unauthenticated range precisely so its bytes are absent.
        if element.kind == CoverageKind::Generation && !element.origin_authenticated {
            unauthenticated += 1;
        }
        match element.kind {
            CoverageKind::Hole => holes += 1,
            CoverageKind::Withheld => {
                withheld += 1;
                if export.scope != ExportScope::Public {
                    // Only a public export withholds; a raw export that claims
                    // to withhold is misdescribing itself.
                    return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
                }
                let carried = dir.join("generations").join(&element.generation_id);
                if carried.exists() {
                    return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
                }
            }
            CoverageKind::Generation => {
                let generation_dir = if export.schema == EXPORT_SCHEMA_V1 {
                    dir.to_path_buf()
                } else {
                    dir.join("generations").join(&element.generation_id)
                };
                let journal = generation_dir.join("journal.jsonl");
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
                generations_verified += 1;
            }
        }
        expected_seq = element.last_seq.saturating_add(1);
        previous_tag = Some(element.final_tag.clone());
    }

    if expected_seq.saturating_sub(1) != export.global_last_seq {
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }
    if export.scope == ExportScope::Public && unauthenticated > 0 {
        // A public export must not carry an unauthenticated generation.
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }
    if export.contains_unauthenticated_legacy != (unauthenticated > 0) {
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }
    if export.complete != (holes == 0 && withheld == 0) {
        return Err(AuditError::Poisoned(PoisonReason::ExportCoverageInvalid));
    }

    Ok(ExportVerification {
        seal_id: export.seal_id,
        schema: export.schema,
        scope: export.scope,
        contains_unauthenticated_legacy: export.contains_unauthenticated_legacy,
        withheld,
        complete: export.complete,
        generations_verified,
        holes,
        global_first_seq: export.global_first_seq,
        global_last_seq: export.global_last_seq,
        unauthenticated_generations: unauthenticated,
        witness_state: export.witness_state,
    })
}
