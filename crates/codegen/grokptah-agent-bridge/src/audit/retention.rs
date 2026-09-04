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

use super::documents::*;
use super::files;
use super::keys::sha256_hex;
use super::ledger::{AuditEntryInput, AuditLedger, CrashPoint};
use super::{AuditError, AuditResult, PoisonReason, RefuseReason};

#[derive(Debug, Clone)]
pub struct RetentionRequest {
    pub generation_id: String,
    /// Seal id of a verified export that already preserved this range.
    pub export_seal_id: Option<String>,
    /// Operator override for retaining a generation that was never exported.
    /// Recorded permanently in the tombstone.
    pub allow_unexported: bool,
    pub reason: RetentionReason,
}

impl RetentionRequest {
    pub fn new(generation_id: impl Into<String>) -> Self {
        Self {
            generation_id: generation_id.into(),
            export_seal_id: None,
            allow_unexported: false,
            reason: RetentionReason::OperatorRetention,
        }
    }

    pub fn with_export_seal(mut self, seal_id: impl Into<String>) -> Self {
        self.export_seal_id = Some(seal_id.into());
        self
    }

    pub fn allow_unexported(mut self) -> Self {
        self.allow_unexported = true;
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
    pub export_seal_id: Option<String>,
    pub allow_unexported: bool,
}

impl AuditLedger {
    pub fn retain(&self, request: RetentionRequest) -> AuditResult<RetentionReceipt> {
        if let Some(poison) = self.is_poisoned() {
            return Err(AuditError::Poisoned(poison));
        }
        let manifest = self.manifest_snapshot();
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
        if request.export_seal_id.is_none() && !request.allow_unexported {
            return Err(AuditError::Refused(RefuseReason::GenerationUnexported));
        }

        // T1: verify completely before promising anything about these bytes.
        let journal = Self::journal_path(self.root(), &descriptor.generation_id);
        let bytes = files::read_bytes(&journal)?;
        let journal_sha256 = sha256_hex(&bytes);
        if descriptor.journal_sha256.as_deref() != Some(journal_sha256.as_str()) {
            return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
        }
        let verification = self.verify_generation(&descriptor.generation_id)?;
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

        // T2: intent before the effect boundary.
        self.append(
            AuditEntryInput::new(
                "audit.retention",
                EntryPhase::Intent,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::RetentionIntent)
            .with_scope(&descriptor.generation_id),
        )?;

        // T3: commit the tombstone. Bytes are still on disk after this returns.
        let mut manifest = self.manifest_snapshot();
        manifest.retention_epoch = manifest.retention_epoch.saturating_add(1);
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
            export_seal_id: request.export_seal_id.clone(),
            allow_unexported: request.allow_unexported,
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
        self.commit_manifest(manifest)?;
        self.cut(CrashPoint::T3Committed)?;

        // T4: remove the bytes the committed manifest authorized removing.
        let dir = Self::generation_dir(self.root(), &descriptor.generation_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|error| AuditError::Io(format!("retention removal: {error}")))?;
            files::fsync_dir(&self.root().join("generations"))?;
        }
        self.cut(CrashPoint::T4Removed)?;

        // T5: mark the removal complete.
        let mut manifest = self.manifest_snapshot();
        if let Some(tombstone) = manifest
            .tombstones
            .iter_mut()
            .find(|t| t.generation_id == descriptor.generation_id)
        {
            tombstone.removed_at = Some(Utc::now());
        }
        self.commit_manifest(manifest)?;

        // T6: pair the intent so open intents return to zero.
        self.append(
            AuditEntryInput::new(
                "audit.retention",
                EntryPhase::Outcome,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::RetentionOutcome)
            .with_scope(&descriptor.generation_id),
        )?;

        Ok(RetentionReceipt {
            generation_id: descriptor.generation_id,
            first_seq: descriptor.first_seq,
            last_seq: descriptor.last_seq,
            entry_count: descriptor.entry_count,
            bytes_removed: descriptor.journal_bytes,
            retention_epoch,
            export_seal_id: request.export_seal_id,
            allow_unexported: request.allow_unexported,
        })
    }
}
