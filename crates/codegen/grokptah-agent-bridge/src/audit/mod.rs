//! Durable append-only audit generations (`grokptah-audit.v2`, #443).
//!
//! # Why this exists
//!
//! The shipped audit ledger in [`crate::orchestration`] appends to
//! `audit/audit.jsonl` and, at 4 MiB, renames it onto `audit.jsonl.1` after
//! deleting whatever `audit.jsonl.1` already held. That two-generation ring
//! destroys the third-oldest generation with no manifest, tombstone, marker or
//! audit record; a crash between the delete and the rename loses the previous
//! generation while the current one survives; and a crash between the rename
//! and the first append leaves no `audit.jsonl` at all, so "rotated" and
//! "never audited" become indistinguishable. Entries carry no sequence and no
//! MAC, so truncation, reordering and substitution are all undetectable.
//!
//! # What this provides
//!
//! - **Forward-only generations.** Rotation never renames or truncates a
//!   journal. It creates the next generation's directory, empty journal and
//!   anchor *first*, then switches an authenticated manifest pointer with one
//!   atomic rename. That rename is the single commit point: a crash on either
//!   side of it has exactly one correct answer.
//! - **A global chain.** `tag = HMAC(K_chain, prev || canonical(record))`,
//!   where the record includes its generation and sequence, so renumbering an
//!   entry or moving one between generations is detectable. Each generation's
//!   `chainBase` is its predecessor's `finalTag`, so the chain is continuous
//!   across rotation, restart and retention.
//! - **Exact sequence continuation.** The next sequence comes from the
//!   authenticated anchor, not from a reserved block, so a restart continues
//!   exactly rather than jumping.
//! - **Tombstone-first retention.** The only deletion path. It commits a
//!   permanent tombstone carrying the deleted range's `firstSeq`, `lastSeq`,
//!   `chainBase` and `finalTag` *before* removing bytes, so the chain stays
//!   verifiable across the hole and deleted history is provably deleted by an
//!   authorized transaction rather than merely missing.
//! - **Honest exports.** v1 for a never-rotated, fully authenticated ledger;
//!   v2 with explicit coverage tiling otherwise; a mandatory refusal rather
//!   than a v1 document that cannot represent a hole.
//!
//! # What this does not provide
//!
//! Joint rollback of every local file to a coherent earlier snapshot satisfies
//! every invariant here. Detecting it needs a platform monotonic counter or a
//! remote witness; [`witness`] defines only the seam, and the default
//! [`witness::UnwitnessedBoundary`] reports honestly rather than implying a
//! guarantee that does not exist.

mod canon;
pub mod documents;
mod export;
mod files;
mod import;
mod keys;
mod ledger;
mod retention;
pub mod witness;

#[cfg(test)]
mod tests;

pub use documents::{
    Anchor, AuditRecord, EntryOutcome, EntryPhase, EntryReason, GapRecord, GenerationDescriptor,
    GenerationState, Manifest, RecoveryEvidence, RetentionReason, RotationReason, SequenceOrigin,
    Tombstone, MAX_LINE_BYTES,
};
pub use export::{
    verify_export, CoverageElement, CoverageKind, ExportFormat, ExportManifest, ExportReceipt,
    ExportVerification,
};
pub use keys::AuditKeys;
pub use ledger::{
    AuditEntryInput, AuditLedger, AuditLedgerOptions, AuditStatus, GenerationVerification,
    RecoverySummary,
};
pub use retention::{RetentionReceipt, RetentionRequest};
pub use witness::{AuditWitness, UnwitnessedBoundary, WitnessBeacon, WitnessState, WitnessVerdict};

pub type AuditResult<T> = Result<T, AuditError>;

/// Terminal integrity failures. The ledger never auto-repairs one of these and
/// never downgrades it into a warning: an audited host must fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoisonReason {
    ManifestMacMismatch,
    ManifestUnknownSchema,
    ManifestTmpPresent,
    ManifestAbsentWithGenerations,
    GenerationIndexDiscontinuity,
    SequenceDiscontinuity,
    ChainDiscontinuity,
    ActiveGenerationInvalid,
    ActiveJournalTruncated,
    AnchorMacMismatch,
    AnchorGenerationMismatch,
    GapMacMismatch,
    EntryMacMismatch,
    EntrySequenceBreak,
    EntryForeignGeneration,
    EntryMalformed,
    SealedGenerationChanged,
    OversizedLine,
    OrphanGenerationNotEmpty,
    TombstoneInconsistent,
    ExportMacMismatch,
    ExportCoverageInvalid,
    SymlinkedPath,
    ConcurrentWriter,
    KeyUnavailable,
    RollbackDetected,
    PartialPersistence,
}

impl PoisonReason {
    /// Stable, secret-free operator code.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ManifestMacMismatch => "manifest_mac_mismatch",
            Self::ManifestUnknownSchema => "manifest_unknown_schema",
            Self::ManifestTmpPresent => "manifest_tmp_present",
            Self::ManifestAbsentWithGenerations => "manifest_absent_with_generations",
            Self::GenerationIndexDiscontinuity => "generation_index_discontinuity",
            Self::SequenceDiscontinuity => "sequence_discontinuity",
            Self::ChainDiscontinuity => "chain_discontinuity",
            Self::ActiveGenerationInvalid => "active_generation_invalid",
            Self::ActiveJournalTruncated => "active_journal_truncated",
            Self::AnchorMacMismatch => "anchor_mac_mismatch",
            Self::AnchorGenerationMismatch => "anchor_generation_mismatch",
            Self::GapMacMismatch => "gap_mac_mismatch",
            Self::EntryMacMismatch => "entry_mac_mismatch",
            Self::EntrySequenceBreak => "entry_sequence_break",
            Self::EntryForeignGeneration => "entry_foreign_generation",
            Self::EntryMalformed => "entry_malformed",
            Self::SealedGenerationChanged => "sealed_generation_changed",
            Self::OversizedLine => "oversized_line",
            Self::OrphanGenerationNotEmpty => "orphan_generation_not_empty",
            Self::TombstoneInconsistent => "tombstone_inconsistent",
            Self::ExportMacMismatch => "export_mac_mismatch",
            Self::ExportCoverageInvalid => "export_coverage_invalid",
            Self::SymlinkedPath => "symlinked_path",
            Self::ConcurrentWriter => "concurrent_writer",
            Self::KeyUnavailable => "key_unavailable",
            Self::RollbackDetected => "rollback_detected",
            Self::PartialPersistence => "partial_persistence",
        }
    }
}

impl std::fmt::Display for PoisonReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Refusals: the ledger is intact, the requested operation is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefuseReason {
    OpenIntentsPresent,
    GenerationUnknown,
    GenerationIsActive,
    GenerationTombstoned,
    GenerationUnexported,
    ExportDestinationExists,
    ExportV1IncompatibleMultiGeneration,
    EntryTooLarge,
}

impl RefuseReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OpenIntentsPresent => "open_intents_present",
            Self::GenerationUnknown => "generation_unknown",
            Self::GenerationIsActive => "generation_is_active",
            Self::GenerationTombstoned => "generation_tombstoned",
            Self::GenerationUnexported => "generation_unexported",
            Self::ExportDestinationExists => "export_destination_exists",
            Self::ExportV1IncompatibleMultiGeneration => "export_v1_incompatible_multi_generation",
            Self::EntryTooLarge => "entry_too_large",
        }
    }
}

impl std::fmt::Display for RefuseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("audit io: {0}")]
    Io(String),
    #[error("audit ledger poisoned: {0}")]
    Poisoned(PoisonReason),
    #[error("audit refused: {0}")]
    Refused(RefuseReason),
    /// Deterministic crash injection. Test builds only — there is no injection
    /// state in a shipped binary.
    #[cfg(test)]
    #[error("test crash cut")]
    CrashCut,
}

impl AuditError {
    /// Stable, secret-free code for operator surfaces and health projections.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "audit_io",
            Self::Poisoned(reason) => reason.as_str(),
            Self::Refused(reason) => reason.as_str(),
            #[cfg(test)]
            Self::CrashCut => "test_crash_cut",
        }
    }

    pub fn poison_reason(&self) -> Option<PoisonReason> {
        match self {
            Self::Poisoned(reason) => Some(*reason),
            _ => None,
        }
    }
}
