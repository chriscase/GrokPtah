//! Tombstone-first retention (#443).
//!
//! The only path in the audit authority that deletes bytes. It commits the
//! record of the deletion *before* the deletion, refuses anything but a sealed
//! non-current generation, and refuses any generation it cannot verify right
//! now — you may not tombstone evidence you cannot currently vouch for.
//!
//! The tombstone keeps `firstSeq`, `lastSeq`, `chainBase` and `finalTag`
//! permanently, so the global chain stays verifiable *across* the hole. Deleted
//! history is provably deleted by an authorized transaction at a named
//! retention epoch, never merely missing.

use chrono::Utc;

use super::authority::{AuditCapability, AuthorityGrant, AuthoritySource};
use super::documents::*;
use super::files;
use super::keys::sha256_hex;
use super::ledger::{AuditEntryInput, AuditLedger, CrashPoint};
use super::{AuditError, AuditResult, PoisonReason, RefuseReason};

/// What entitles this deletion. There is no third option and no default:
/// every call must say which one it is relying on.
#[derive(Debug, Clone)]
pub enum RetentionBasis {
    /// A seal **this ledger issued and re-verified**, which must be found in
    /// the manifest's seal registry and must have actually carried the exact
    /// range being deleted. A caller-supplied id is a lookup key, never a
    /// claim: an unknown id, or one whose export withheld or holed the range,
    /// is refused.
    ExportedUnder { seal_id: String },
    /// A verified single-use capability grant for
    /// [`AuditCapability::RetainUnexported`]. This is the only way to destroy
    /// the last copy of a range, and the grant id and its source are recorded
    /// permanently in the tombstone.
    Grant(Box<AuthorityGrant>),
}

#[derive(Debug, Clone)]
pub struct RetentionRequest {
    pub generation_id: String,
    pub basis: RetentionBasis,
    pub reason: RetentionReason,
}

impl RetentionRequest {
    /// Retain a generation a verified export already carried.
    pub fn exported_under(generation_id: impl Into<String>, seal_id: impl Into<String>) -> Self {
        Self {
            generation_id: generation_id.into(),
            basis: RetentionBasis::ExportedUnder {
                seal_id: seal_id.into(),
            },
            reason: RetentionReason::OperatorRetention,
        }
    }

    /// Retain a generation no export ever carried, under a verified grant.
    pub fn under_grant(generation_id: impl Into<String>, grant: AuthorityGrant) -> Self {
        Self {
            generation_id: generation_id.into(),
            basis: RetentionBasis::Grant(Box::new(grant)),
            reason: RetentionReason::OperatorRetention,
        }
    }

    pub fn with_reason(mut self, reason: RetentionReason) -> Self {
        self.reason = reason;
        self
    }
}

#[derive(Debug, Clone)]
pub struct RetentionReceipt {
    pub generation_id: String,
    pub first_seq: u64,
    pub last_seq: u64,
    pub entry_count: u64,
    pub bytes_removed: u64,
    pub retention_epoch: u64,
    /// The seal that carried this range, when the basis was an export.
    pub export_seal_id: Option<String>,
    /// `true` only when the deletion destroyed the last copy of the range.
    pub allow_unexported: bool,
    pub authority_grant_id: Option<String>,
    pub authority_source: Option<AuthoritySource>,
}

impl AuditLedger {
    /// Tombstone-first retention, entirely inside one structural barrier.
    ///
    /// Taking a manifest snapshot, verifying, then committing a manifest built
    /// from that snapshot let a rotation commit in between and be silently
    /// overwritten — dropping a committed generation and regressing the epoch.
    pub fn retain(&self, request: RetentionRequest) -> AuditResult<RetentionReceipt> {
        let mut tx = self.structural_tx();
        if let Some(poison) = tx.poisoned() {
            return Err(AuditError::Poisoned(poison));
        }
        let manifest = tx.manifest_clone();
        let descriptor = manifest
            .generation(&request.generation_id)
            .ok_or(AuditError::Refused(RefuseReason::GenerationUnknown))?
            .clone();

        // T0 preconditions.
        if descriptor.generation_id == manifest.active_generation_id {
            return Err(AuditError::Refused(RefuseReason::GenerationIsActive));
        }
        match descriptor.state {
            GenerationState::Active => {
                return Err(AuditError::Refused(RefuseReason::GenerationIsActive))
            }
            GenerationState::Tombstoned => {
                return Err(AuditError::Refused(RefuseReason::GenerationTombstoned))
            }
            GenerationState::Sealed => {}
        }
        let active = manifest.active()?;
        if descriptor.index >= active.index {
            return Err(AuditError::Refused(RefuseReason::GenerationIsActive));
        }
        // T1: verify completely before promising anything about these bytes.
        let journal = Self::journal_path(self.root(), &descriptor.generation_id);
        let bytes = files::read_bytes(&journal)?;
        let journal_sha256 = sha256_hex(&bytes);
        if descriptor.journal_sha256.as_deref() != Some(journal_sha256.as_str()) {
            return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
        }
        let verification = tx.verify_generation(&descriptor.generation_id)?;
        if verification.last_seq != descriptor.last_seq
            || verification.final_tag.as_str()
                != descriptor.final_tag.as_deref().unwrap_or_default()
        {
            return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
        }
        // The successor must still chain over the range about to become a hole.
        let successor = manifest
            .generations
            .iter()
            .find(|g| g.index == descriptor.index.saturating_add(1))
            .ok_or(AuditError::Poisoned(PoisonReason::ChainDiscontinuity))?;
        if successor.chain_base.as_str() != verification.final_tag.as_str()
            || successor.first_seq != descriptor.last_seq.saturating_add(1)
        {
            return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
        }

        // T1b: establish the authority *after* verifying, so a seal is matched
        // against facts this transaction just re-established rather than
        // against the descriptor's own claims about itself.
        let seal_match = match &request.basis {
            RetentionBasis::ExportedUnder { seal_id } => {
                let seal = manifest
                    .seals
                    .iter()
                    .find(|seal| &seal.seal_id == seal_id)
                    .ok_or(AuditError::Refused(RefuseReason::ExportSealUnknown))?;
                // Carrying the *range* is the claim. A seal whose coverage
                // withheld or holed this generation proves nothing about it,
                // and `carried` never records those elements at all.
                let carried = seal
                    .carried
                    .iter()
                    .find(|range| range.generation_id == descriptor.generation_id)
                    .ok_or(AuditError::Refused(RefuseReason::ExportSealDoesNotCover))?;
                if carried.first_seq != descriptor.first_seq
                    || carried.last_seq != verification.last_seq
                    || carried.final_tag != verification.final_tag
                    || carried.journal_sha256 != journal_sha256
                    || carried.entry_count != verification.entry_count
                {
                    // The bytes changed since the export, so the export is not
                    // a copy of what is about to be deleted.
                    return Err(AuditError::Refused(RefuseReason::ExportSealDoesNotCover));
                }
                Some(seal_id.clone())
            }
            RetentionBasis::Grant(_) => None,
        };

        // T2: intent before the effect boundary.
        tx.append(
            AuditEntryInput::new(
                "audit.retention",
                EntryPhase::Intent,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::RetentionIntent)
            .with_producer(&descriptor.generation_id)
            .with_scope(&descriptor.generation_id),
            None,
        )?;

        // T3: commit the tombstone. Bytes are still on disk after this returns.
        let mut manifest = tx.manifest_clone();
        // The grant is spent in the same manifest write that commits the
        // tombstone: never spent without the deletion, never deleted with the
        // grant still spendable.
        let consumed = match &request.basis {
            RetentionBasis::Grant(grant) => Some(tx.stage_authority(
                &mut manifest,
                grant,
                AuditCapability::RetainUnexported,
                &descriptor.generation_id,
            )?),
            RetentionBasis::ExportedUnder { .. } => None,
        };
        manifest.retention_epoch = manifest
            .retention_epoch
            .checked_add(1)
            .ok_or(AuditError::Poisoned(PoisonReason::SequenceExhausted))?;
        let retention_epoch = manifest.retention_epoch;
        let now = Utc::now();
        manifest.tombstones.push(Tombstone {
            generation_id: descriptor.generation_id.clone(),
            index: descriptor.index,
            first_seq: descriptor.first_seq,
            last_seq: descriptor.last_seq,
            entry_count: descriptor.entry_count,
            journal_bytes: descriptor.journal_bytes,
            journal_sha256,
            chain_base: descriptor.chain_base.clone(),
            final_tag: verification.final_tag.clone(),
            key_id: descriptor.key_id.clone(),
            retention_epoch,
            reason: request.reason,
            export_seal_id: seal_match.clone(),
            allow_unexported: consumed.is_some(),
            authority_grant_id: consumed.as_ref().map(|c| c.grant_id.clone()),
            authority_source: consumed.as_ref().map(|c| c.source),
            committed_at: now,
            removed_at: None,
        });
        {
            let target = manifest
                .generation_mut(&descriptor.generation_id)
                .ok_or(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid))?;
            target.state = GenerationState::Tombstoned;
            target.tombstoned_at = Some(now);
        }
        tx.commit_manifest(manifest)?;
        tx.cut(CrashPoint::T3Committed)?;

        // T4: remove the bytes the committed manifest authorized removing.
        let dir = Self::generation_dir(self.root(), &descriptor.generation_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|error| AuditError::Io(format!("retention removal: {error}")))?;
            files::fsync_dir(&self.root().join("generations"))?;
        }
        tx.cut(CrashPoint::T4Removed)?;

        // T5: mark the removal complete.
        let mut manifest = tx.manifest_clone();
        if let Some(tombstone) = manifest
            .tombstones
            .iter_mut()
            .find(|t| t.generation_id == descriptor.generation_id)
        {
            tombstone.removed_at = Some(Utc::now());
        }
        tx.commit_manifest(manifest)?;

        // T6: pair the intent so open intents return to zero.
        tx.append(
            AuditEntryInput::new(
                "audit.retention",
                EntryPhase::Outcome,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::RetentionOutcome)
            .with_producer(&descriptor.generation_id)
            .with_scope(&descriptor.generation_id),
            None,
        )?;

        Ok(RetentionReceipt {
            generation_id: descriptor.generation_id,
            first_seq: descriptor.first_seq,
            last_seq: descriptor.last_seq,
            entry_count: descriptor.entry_count,
            bytes_removed: descriptor.journal_bytes,
            retention_epoch,
            export_seal_id: seal_match,
            allow_unexported: consumed.is_some(),
            authority_grant_id: consumed.as_ref().map(|c| c.grant_id.clone()),
            authority_source: consumed.map(|c| c.source),
        })
    }
}
