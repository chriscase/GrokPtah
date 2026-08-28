//! Authenticated audit documents: manifest, generation descriptors, tombstones,
//! anchors, journal records, and the durable dropped-entry gap file (#443).
//!
//! Every document is `deny_unknown_fields` and carries its own MAC over the
//! canonical bytes of itself minus that MAC.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::canon::canonical_bytes_without_mac;
use super::keys::AuditKeys;
use super::{AuditError, AuditResult, PoisonReason};

pub const MANIFEST_SCHEMA: &str = "grokptah-audit-manifest.v2";
pub const ANCHOR_SCHEMA: &str = "grokptah-audit-anchor.v2";
pub const GAP_SCHEMA: &str = "grokptah-audit-gap.v2";
pub const EXPORT_SCHEMA_V1: &str = "grokptah-audit-export.v1";
pub const EXPORT_SCHEMA_V2: &str = "grokptah-audit-export.v2";
pub const MANIFEST_VERSION: u32 = 2;
pub const RECORD_VERSION: u32 = 2;

/// Upper bound on one journal line, including its newline.
pub const MAX_LINE_BYTES: usize = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationState {
    Active,
    Sealed,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationReason {
    Bytes,
    Entries,
    Age,
    KeyRotation,
    Operator,
    Recovery,
    LegacyImport,
    Genesis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequenceOrigin {
    /// Issued by this ledger, one per committed append.
    Issued,
    /// Assigned while importing legacy v1 bytes that never carried sequences.
    ImportAssigned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionReason {
    OperatorRetention,
    PolicyAge,
    PolicyBytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPhase {
    Intent,
    Outcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryOutcome {
    Accepted,
    Rejected,
    Uncertain,
}

/// Closed reason vocabulary. Free text never reaches the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryReason {
    HostRestartInterrupted,
    GenerationSealing,
    GenerationOpened,
    RecoveryTornTail,
    RecoveryDroppedEntries,
    RetentionIntent,
    RetentionOutcome,
    LegacyImported,
    /// Legacy v1 bytes changed after the v2 cutover: an older binary wrote to
    /// the retired ledger. Recorded, never repaired.
    LegacyWrittenAfterCutover,
    HostShutdown,
    Unauthenticated,
    ForbiddenScope,
    WorkspaceMismatch,
    SessionBusy,
    StaleRevision,
    CursorExpired,
    CapacityExhausted,
    Timeout,
    InvalidRequest,
    Unsupported,
    Conflict,
    Internal,
}

/// Maximum length of a producer code.
pub const MAX_CODE_BYTES: usize = 64;

/// Constrain a producer op to `[a-z0-9_.-]{1,64}`.
///
/// `op` was previously arbitrary text truncated to 64 bytes, so a producer
/// could put a secret fragment in the durable record. The shape admits no path
/// separator, whitespace, quote, or control character, so it cannot carry a
/// path, a prompt, or a credential. Tool names and the ledger's own dotted ops
/// fit it; anything else is replaced rather than truncated, because a truncated
/// secret is still a secret.
pub fn sanitize_op(op: &str) -> String {
    let acceptable = !op.is_empty()
        && op.len() <= MAX_CODE_BYTES
        && op.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'.' || b == b'-'
        });
    if acceptable {
        op.to_string()
    } else {
        "invalid_op".to_string()
    }
}

/// Constrain a producer code to `[a-z0-9_]{1,64}`.
///
/// Anything else is replaced rather than truncated: a truncated path is still
/// a path fragment, whereas `invalid_code` carries no caller-controlled bytes.
pub fn sanitize_code(code: &str) -> String {
    let acceptable = !code.is_empty()
        && code.len() <= MAX_CODE_BYTES
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if acceptable {
        code.to_string()
    } else {
        "invalid_code".to_string()
    }
}

/// Byte-exact evidence for a recovery that changed the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryEvidence {
    /// Exact number of bytes removed or lost.
    pub bytes: u64,
    /// SHA-256 of exactly those bytes, so the evidence is checkable.
    pub sha256: String,
    /// Byte offset in the journal at which the evidence starts.
    pub at_offset: u64,
    /// Number of entries known to be missing, when known.
    pub lost_entries: u64,
}

/// One journal line. `gen` and `seq` are inside the MAC input, which is what
/// makes renumbering an entry and moving one between generations detectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuditRecord {
    pub v: u32,
    #[serde(rename = "gen")]
    pub generation: String,
    pub seq: u64,
    pub ts: DateTime<Utc>,
    pub op: String,
    pub phase: EntryPhase,
    pub outcome: EntryOutcome,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub reason: Option<EntryReason>,
    /// Producer error/outcome code, constrained to `[a-z0-9_]{1,64}`.
    ///
    /// A closed *shape* rather than a closed vocabulary: it cannot contain a
    /// path separator, whitespace, a quote, or a dot, so it provably cannot
    /// carry a filesystem path, a prompt, or a credential. Producers whose
    /// codes are already an enum (`OrchErrorCode`) round-trip exactly.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub code: Option<String>,
    /// Keyed digest of the stable producer intent identity, so an intent and
    /// its outcome can be correlated across restarts without exposing the id.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub producer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub actor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub request: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub authz_rev: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub cap_rev: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub policy_rev: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub recovery: Option<RecoveryEvidence>,
    pub prev: String,
    pub tag: String,
}

impl AuditRecord {
    /// `tag = HMAC(K_chain, prev || canonical(record without "tag"))`.
    pub(crate) fn compute_tag(&self, keys: &AuditKeys) -> AuditResult<String> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| AuditError::Io(format!("record to value: {error}")))?;
        match value.as_object_mut() {
            Some(map) => {
                map.remove("tag");
            }
            None => return Err(AuditError::Io("record is not an object".into())),
        }
        let payload = super::canon::canonical_value_bytes(&value)?;
        Ok(keys.chain_tag(&self.prev, &payload))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationDescriptor {
    pub generation_id: String,
    pub index: u32,
    pub state: GenerationState,
    pub key_id: String,
    pub predecessor_id: Option<String>,
    pub chain_base: String,
    pub first_seq: u64,
    /// Exact for sealed and tombstoned generations; a floor for the active one.
    pub last_seq: u64,
    pub entry_count: u64,
    pub journal_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub journal_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub final_tag: Option<String>,
    pub rotation_reason: RotationReason,
    pub sequence_origin: SequenceOrigin,
    /// `false` for imported legacy bytes: preserved, never vouched for.
    pub origin_authenticated: bool,
    /// `true` when v1 already destroyed generations older than this one.
    pub preceding_loss_unknown: bool,
    pub opened_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sealed_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tombstoned_at: Option<DateTime<Utc>>,
}

/// Permanent record of an authorized deletion. Never removed from the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tombstone {
    pub generation_id: String,
    pub index: u32,
    pub first_seq: u64,
    pub last_seq: u64,
    pub entry_count: u64,
    pub journal_bytes: u64,
    pub journal_sha256: String,
    pub chain_base: String,
    pub final_tag: String,
    pub key_id: String,
    pub retention_epoch: u64,
    pub reason: RetentionReason,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub export_seal_id: Option<String>,
    pub allow_unexported: bool,
    pub committed_at: DateTime<Utc>,
    /// `None` between the tombstone commit and byte removal.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub removed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub manifest_version: u32,
    pub installation_id: String,
    pub key_id: String,
    pub key_epoch: u32,
    pub manifest_epoch: u64,
    pub retention_epoch: u64,
    pub active_generation_id: String,
    pub global_first_seq: u64,
    /// `active.last_seq` as of this manifest write. A floor, never exact.
    pub global_last_seq_floor: u64,
    pub generations: Vec<GenerationDescriptor>,
    pub tombstones: Vec<Tombstone>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub mac: String,
}

impl Manifest {
    pub(crate) fn seal(&mut self, keys: &AuditKeys) -> AuditResult<()> {
        self.mac = String::new();
        let payload = canonical_bytes_without_mac(&*self)?;
        self.mac = keys.manifest_mac(&payload);
        Ok(())
    }

    pub(crate) fn verify(&self, keys: &AuditKeys) -> AuditResult<()> {
        if self.schema != MANIFEST_SCHEMA || self.manifest_version != MANIFEST_VERSION {
            return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
        }
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.manifest_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Poisoned(PoisonReason::ManifestMacMismatch));
        }
        Ok(())
    }

    pub(crate) fn generation(&self, generation_id: &str) -> Option<&GenerationDescriptor> {
        self.generations
            .iter()
            .find(|g| g.generation_id == generation_id)
    }

    pub(crate) fn generation_mut(
        &mut self,
        generation_id: &str,
    ) -> Option<&mut GenerationDescriptor> {
        self.generations
            .iter_mut()
            .find(|g| g.generation_id == generation_id)
    }

    pub(crate) fn active(&self) -> AuditResult<&GenerationDescriptor> {
        self.generation(&self.active_generation_id)
            .ok_or(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Anchor {
    pub schema: String,
    pub generation_id: String,
    pub key_id: String,
    pub last_seq: u64,
    pub last_tag: String,
    pub journal_bytes: u64,
    /// Producer digests of intents recorded without a matching outcome.
    ///
    /// A set rather than a counter: an intent must be closed by *its own*
    /// outcome. A bare counter let any later outcome — a shutdown record, say —
    /// silently close an unrelated intent.
    pub open_intent_ids: Vec<String>,
    /// `true` once more than [`MAX_TRACKED_INTENTS`] intents were open at once.
    /// The entries are still recorded; only the correlation set is bounded, and
    /// the limit is stated rather than hidden.
    pub intent_tracking_overflowed: bool,
    pub updated_at: DateTime<Utc>,
    pub mac: String,
}

/// Upper bound on simultaneously tracked open intents.
pub const MAX_TRACKED_INTENTS: usize = 256;

impl Anchor {
    pub(crate) fn seal(&mut self, keys: &AuditKeys) -> AuditResult<()> {
        self.mac = String::new();
        let payload = canonical_bytes_without_mac(&*self)?;
        self.mac = keys.anchor_mac(&payload);
        Ok(())
    }

    pub(crate) fn verify(&self, keys: &AuditKeys, generation_id: &str) -> AuditResult<()> {
        if self.schema != ANCHOR_SCHEMA {
            return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
        }
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.anchor_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Poisoned(PoisonReason::AnchorMacMismatch));
        }
        if self.generation_id != generation_id {
            return Err(AuditError::Poisoned(PoisonReason::AnchorGenerationMismatch));
        }
        Ok(())
    }
}

/// Durable evidence that producer-side entries were dropped before they could
/// be appended. The legacy ledger held this only in memory, so a restart
/// erased the evidence that evidence had been lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GapRecord {
    pub generation_id: String,
    pub after_seq: u64,
    pub lost_entries: u64,
    pub reason: EntryReason,
    pub recorded_at: DateTime<Utc>,
    /// `true` once the loss has also been written into the chained journal.
    pub journaled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GapFile {
    pub schema: String,
    pub gaps: Vec<GapRecord>,
    pub updated_at: DateTime<Utc>,
    pub mac: String,
}

impl GapFile {
    pub(crate) fn new() -> Self {
        Self {
            schema: GAP_SCHEMA.to_string(),
            gaps: Vec::new(),
            updated_at: Utc::now(),
            mac: String::new(),
        }
    }

    pub(crate) fn seal(&mut self, keys: &AuditKeys) -> AuditResult<()> {
        self.mac = String::new();
        let payload = canonical_bytes_without_mac(&*self)?;
        self.mac = keys.anchor_mac(&payload);
        Ok(())
    }

    pub(crate) fn verify(&self, keys: &AuditKeys) -> AuditResult<()> {
        if self.schema != GAP_SCHEMA {
            return Err(AuditError::Poisoned(PoisonReason::ManifestUnknownSchema));
        }
        let payload = canonical_bytes_without_mac(self)?;
        let expected = keys.anchor_mac(&payload);
        if !crate::orchestration::constant_time_eq(expected.as_bytes(), self.mac.as_bytes()) {
            return Err(AuditError::Poisoned(PoisonReason::GapMacMismatch));
        }
        Ok(())
    }
}

pub(crate) fn generation_id(index: u32) -> String {
    format!("g-{index:06}")
}

pub(crate) fn valid_generation_id(value: &str) -> bool {
    value.len() == 8 && value.starts_with("g-") && value[2..].bytes().all(|b| b.is_ascii_digit())
}
