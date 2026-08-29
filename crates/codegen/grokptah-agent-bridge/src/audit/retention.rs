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
use super::{AuditError, PoisonReason, RefuseReason};

/// How far a retention transaction got, from the caller's point of view.
///
/// T3 — the tombstone commit — is the effect boundary. A bare `Err` on either
/// side of it looks identical to a caller, which is exactly the thing an audit
/// authority must not do: "nothing was deleted" and "the deletion is committed
/// and may be half-applied" are different facts and demand different responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionPhase {
    /// The tombstone was never committed. The generation is untouched, and a
    /// retry is a fresh attempt rather than a resumption.
    NotCommitted,
    /// The tombstone is committed and the bytes are gone.
    Committed,
    /// The tombstone is committed; whether the bytes are gone is unknown.
    /// The deletion is authorized and permanent either way. The next open
    /// resumes it, and a retry converges rather than deleting twice.
    Uncertain,
}

impl RetentionPhase {
    /// Stable, secret-free operator code.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotCommitted => "not_committed",
            Self::Committed => "committed",
            Self::Uncertain => "uncertain",
        }
    }
}

impl std::fmt::Display for RetentionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed retention, carrying the phase it failed in.
#[derive(Debug)]
pub struct RetentionFailure {
    pub phase: RetentionPhase,
    pub source: AuditError,
}

impl RetentionFailure {
    fn not_committed(source: AuditError) -> Self {
        Self {
            phase: RetentionPhase::NotCommitted,
            source,
        }
    }

    /// After T3 nothing can restore the range: the tombstone is committed and
    /// the removal is authorized, so every later failure is `Uncertain`, never
    /// a plain error the caller might read as "no effect".
    fn uncertain(source: AuditError) -> Self {
        Self {
            phase: RetentionPhase::Uncertain,
            source,
        }
    }

    /// Stable, secret-free operator code for the underlying failure.
    pub fn code(&self) -> &'static str {
        self.source.code()
    }
}

impl std::fmt::Display for RetentionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "retention {}: {}", self.phase, self.source.code())
    }
}

impl std::error::Error for RetentionFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

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
    /// Always [`RetentionPhase::Committed`] on the success path. Present so a
    /// caller can log one field rather than inferring the phase from the
    /// shape of the result.
    pub phase: RetentionPhase,
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
    pub fn retain(&self, request: RetentionRequest) -> Result<RetentionReceipt, RetentionFailure> {
        let mut tx = self
            .structural_tx()
            .map_err(RetentionFailure::not_committed)?;
        if let Some(poison) = tx.poisoned() {
            return Err(RetentionFailure::not_committed(AuditError::Poisoned(
                poison,
            )));
        }
        let manifest = tx.manifest_clone();
        let descriptor = manifest
            .generation(&request.generation_id)
            .ok_or_else(|| {
                RetentionFailure::not_committed(AuditError::Refused(
                    RefuseReason::GenerationUnknown,
                ))
            })?
            .clone();

        // T0 preconditions.
        if descriptor.generation_id == manifest.active_generation_id {
            return Err(RetentionFailure::not_committed(AuditError::Refused(
                RefuseReason::GenerationIsActive,
            )));
        }
        match descriptor.state {
            GenerationState::Active => {
                return Err(RetentionFailure::not_committed(AuditError::Refused(
                    RefuseReason::GenerationIsActive,
                )))
            }
            GenerationState::Tombstoned => {
                return Err(RetentionFailure::not_committed(AuditError::Refused(
                    RefuseReason::GenerationTombstoned,
                )))
            }
            GenerationState::Sealed => {}
        }
        let active = manifest.active().map_err(RetentionFailure::not_committed)?;
        if descriptor.index >= active.index {
            return Err(RetentionFailure::not_committed(AuditError::Refused(
                RefuseReason::GenerationIsActive,
            )));
        }
        // T1: verify completely before promising anything about these bytes.
        let journal = Self::journal_path(self.root(), &descriptor.generation_id);
        let bytes = files::read_bytes(&journal).map_err(RetentionFailure::not_committed)?;
        let journal_sha256 = sha256_hex(&bytes);
        if descriptor.journal_sha256.as_deref() != Some(journal_sha256.as_str()) {
            return Err(RetentionFailure::not_committed(AuditError::Poisoned(
                PoisonReason::SealedGenerationChanged,
            )));
        }
        let verification = tx
            .verify_generation(&descriptor.generation_id)
            .map_err(RetentionFailure::not_committed)?;
        if verification.last_seq != descriptor.last_seq
            || verification.final_tag.as_str()
                != descriptor.final_tag.as_deref().unwrap_or_default()
        {
            return Err(RetentionFailure::not_committed(AuditError::Poisoned(
                PoisonReason::SealedGenerationChanged,
            )));
        }
        // The successor must still chain over the range about to become a hole.
        let successor = manifest
            .generations
            .iter()
            .find(|g| g.index == descriptor.index.saturating_add(1))
            .ok_or_else(|| {
                RetentionFailure::not_committed(AuditError::Poisoned(
                    PoisonReason::ChainDiscontinuity,
                ))
            })?;
        if successor.chain_base.as_str() != verification.final_tag.as_str()
            || successor.first_seq != descriptor.last_seq.saturating_add(1)
        {
            return Err(RetentionFailure::not_committed(AuditError::Poisoned(
                PoisonReason::ChainDiscontinuity,
            )));
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
                    .ok_or_else(|| {
                        RetentionFailure::not_committed(AuditError::Refused(
                            RefuseReason::ExportSealUnknown,
                        ))
                    })?;
                // Carrying the *range* is the claim. A seal whose coverage
                // withheld or holed this generation proves nothing about it,
                // and `carried` never records those elements at all.
                let carried = seal
                    .carried
                    .iter()
                    .find(|range| range.generation_id == descriptor.generation_id)
                    .ok_or_else(|| {
                        RetentionFailure::not_committed(AuditError::Refused(
                            RefuseReason::ExportSealDoesNotCover,
                        ))
                    })?;
                if carried.first_seq != descriptor.first_seq
                    || carried.last_seq != verification.last_seq
                    || carried.final_tag != verification.final_tag
                    || carried.journal_sha256 != journal_sha256
                    || carried.entry_count != verification.entry_count
                {
                    // The bytes changed since the export, so the export is not
                    // a copy of what is about to be deleted.
                    return Err(RetentionFailure::not_committed(AuditError::Refused(
                        RefuseReason::ExportSealDoesNotCover,
                    )));
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
        )
        .map_err(RetentionFailure::not_committed)?;

        // T3: commit the tombstone. Bytes are still on disk after this returns.
        let mut manifest = tx.manifest_clone();
        // The grant is spent in the same manifest write that commits the
        // tombstone: never spent without the deletion, never deleted with the
        // grant still spendable.
        let consumed = match &request.basis {
            RetentionBasis::Grant(grant) => Some(
                tx.stage_authority(
                    &mut manifest,
                    grant,
                    AuditCapability::RetainUnexported,
                    &descriptor.generation_id,
                )
                .map_err(RetentionFailure::not_committed)?,
            ),
            RetentionBasis::ExportedUnder { .. } => None,
        };
        manifest.retention_epoch = manifest.retention_epoch.checked_add(1).ok_or_else(|| {
            RetentionFailure::not_committed(AuditError::Poisoned(PoisonReason::SequenceExhausted))
        })?;
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
                .ok_or_else(|| {
                    RetentionFailure::not_committed(AuditError::Poisoned(
                        PoisonReason::ActiveGenerationInvalid,
                    ))
                })?;
            target.state = GenerationState::Tombstoned;
            target.tombstoned_at = Some(now);
        }
        // The effect boundary. Before this line nothing has happened; after
        // it the deletion is authorized and permanent, so no later failure may
        // present itself as "no effect".
        tx.commit_manifest(manifest)
            .map_err(RetentionFailure::not_committed)?;
        tx.cut(CrashPoint::T3Committed)
            .map_err(RetentionFailure::uncertain)?;

        // T4: remove the bytes the committed manifest authorized removing.
        let dir = Self::generation_dir(self.root(), &descriptor.generation_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir).map_err(|error| {
                RetentionFailure::uncertain(AuditError::Io(format!("retention removal: {error}")))
            })?;
            files::fsync_dir(&self.root().join("generations"))
                .map_err(RetentionFailure::uncertain)?;
        }
        tx.cut(CrashPoint::T4Removed)
            .map_err(RetentionFailure::uncertain)?;

        // T5: mark the removal complete.
        let mut manifest = tx.manifest_clone();
        if let Some(tombstone) = manifest
            .tombstones
            .iter_mut()
            .find(|t| t.generation_id == descriptor.generation_id)
        {
            tombstone.removed_at = Some(Utc::now());
        }
        tx.commit_manifest(manifest)
            .map_err(RetentionFailure::uncertain)?;

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
        )
        .map_err(RetentionFailure::uncertain)?;

        Ok(RetentionReceipt {
            phase: RetentionPhase::Committed,
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
