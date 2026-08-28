//! The audit generation ledger: open, recover, append, rotate (#443).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use parking_lot::Mutex;

use super::documents::*;
use super::files;
use super::import::{
    bootstrap_path, plan_legacy_import, write_imported_journal, BootstrapMarker, LegacyImportPlan,
};
use super::keys::{sha256_hex, AuditKeys};
use super::witness::{
    AuditWitness, UnwitnessedBoundary, WitnessBeacon, WitnessState, WitnessVerdict,
};
use super::{AuditError, AuditResult, PoisonReason, RefuseReason};

/// Deterministic crash-injection points. In non-test builds [`AuditLedger::cut`]
/// is a no-op, so no injection state exists in a shipped binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    JournalAppendedBeforeAnchor,
    R1Frozen,
    R2Prepared,
    R3Committed,
    T3Committed,
    T4Removed,
}

/// The caller-facing shape of one audit entry. Raw identifiers are converted to
/// keyed digests on the way in, so a path, prompt or native id cannot reach the
/// journal even if a producer passes one.
#[derive(Debug, Clone)]
pub struct AuditEntryInput {
    pub op: String,
    pub phase: EntryPhase,
    pub outcome: EntryOutcome,
    pub reason: Option<EntryReason>,
    pub actor: Option<String>,
    pub request: Option<String>,
    pub scope: Option<String>,
    pub authz_rev: Option<u64>,
    pub cap_rev: Option<u64>,
    pub policy_rev: Option<u64>,
}

impl AuditEntryInput {
    pub fn new(op: impl Into<String>, phase: EntryPhase, outcome: EntryOutcome) -> Self {
        Self {
            op: op.into(),
            phase,
            outcome,
            reason: None,
            actor: None,
            request: None,
            scope: None,
            authz_rev: None,
            cap_rev: None,
            policy_rev: None,
        }
    }

    pub fn with_reason(mut self, reason: EntryReason) -> Self {
        self.reason = Some(reason);
        self
    }

    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    pub fn with_request(mut self, request: impl Into<String>) -> Self {
        self.request = Some(request.into());
        self
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = Some(scope.into());
        self
    }
}

/// Open-time configuration.
#[derive(Default)]
pub struct AuditLedgerOptions {
    /// Rollback witness boundary. Defaults to [`UnwitnessedBoundary`].
    pub witness: Option<Arc<dyn AuditWitness>>,
    /// Directory holding legacy v1 `audit.jsonl` / `audit.jsonl.1`. Imported
    /// only when this root has never committed a manifest.
    pub legacy_v1_dir: Option<PathBuf>,
}

/// What recovery actually did, so an operator never has to infer it.
#[derive(Debug, Clone, Default)]
pub struct RecoverySummary {
    pub adopted_tail_entries: u64,
    pub torn_tail: Option<RecoveryEvidence>,
    pub orphan_generation: Option<String>,
    pub resumed_removals: Vec<String>,
    pub closed_intents: u64,
    pub durable_gaps: Vec<GapRecord>,
    pub initialized: bool,
    pub imported_generations: usize,
}

#[derive(Debug, Clone)]
pub struct AuditStatus {
    pub installation_id: String,
    pub key_id: String,
    pub manifest_epoch: u64,
    pub retention_epoch: u64,
    pub active_generation_id: String,
    pub global_first_seq: u64,
    pub global_last_seq: u64,
    pub generations: usize,
    pub tombstones: usize,
    pub open_intents: u64,
    pub journal_bytes: u64,
    pub poisoned: Option<PoisonReason>,
    pub witness_state: WitnessState,
    pub imported_generations: usize,
    pub recovery: RecoverySummary,
}

#[derive(Debug, Clone)]
pub struct GenerationVerification {
    pub generation_id: String,
    pub first_seq: u64,
    pub last_seq: u64,
    pub entry_count: u64,
    pub journal_bytes: u64,
    pub journal_sha256: String,
    pub final_tag: String,
    pub origin_authenticated: bool,
}

#[derive(Debug, Clone)]
struct LiveTail {
    generation_id: String,
    last_seq: u64,
    last_tag: String,
    journal_bytes: u64,
    open_intents: u64,
}

struct Inner {
    manifest: Manifest,
    live: LiveTail,
    poisoned: Option<PoisonReason>,
    recovery: RecoverySummary,
    witness_state: WitnessState,
}

pub struct AuditLedger {
    root: PathBuf,
    keys: Arc<AuditKeys>,
    witness: Arc<dyn AuditWitness>,
    inner: Mutex<Inner>,
    #[cfg(test)]
    crash_at: Option<CrashPoint>,
}

impl std::fmt::Debug for AuditLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never renders key material, journal contents, or scope.
        let guard = self.inner.lock();
        f.debug_struct("AuditLedger")
            .field("root", &self.root)
            .field("activeGeneration", &guard.manifest.active_generation_id)
            .field("lastSeq", &guard.live.last_seq)
            .field("openIntents", &guard.live.open_intents)
            .field("poisoned", &guard.poisoned)
            .finish()
    }
}

pub(crate) struct JournalScan {
    pub(crate) last_seq: u64,
    pub(crate) last_tag: String,
    pub(crate) bytes: u64,
    pub(crate) complete_len: u64,
    pub(crate) torn: Option<RecoveryEvidence>,
    pub(crate) entry_count: u64,
}

impl AuditLedger {
    pub fn open(root: impl AsRef<Path>, keys: Arc<AuditKeys>) -> AuditResult<Self> {
        Self::open_with_options(root, keys, AuditLedgerOptions::default())
    }

    pub fn open_with_witness(
        root: impl AsRef<Path>,
        keys: Arc<AuditKeys>,
        witness: Arc<dyn AuditWitness>,
    ) -> AuditResult<Self> {
        Self::open_with_options(
            root,
            keys,
            AuditLedgerOptions {
                witness: Some(witness),
                ..AuditLedgerOptions::default()
            },
        )
    }

    pub fn open_with_options(
        root: impl AsRef<Path>,
        keys: Arc<AuditKeys>,
        options: AuditLedgerOptions,
    ) -> AuditResult<Self> {
        let witness: Arc<dyn AuditWitness> = options
            .witness
            .unwrap_or_else(|| Arc::new(UnwitnessedBoundary));
        let root = root.as_ref().to_path_buf();
        files::create_private_dir_all(&root)?;
        files::reject_symlink(&root)?;
        files::create_private_dir_all(&root.join("generations"))?;

        let mut recovery = RecoverySummary::default();
        let manifest = match Self::load_manifest(&root, &keys)? {
            Some(manifest) => manifest,
            None => {
                recovery.initialized = true;
                let plan = match options.legacy_v1_dir.as_deref() {
                    Some(dir) if dir.exists() => plan_legacy_import(dir)?,
                    _ => LegacyImportPlan::default(),
                };
                recovery.imported_generations = plan.generations.len();
                Self::initialize(&root, &keys, &plan)?
            }
        };
        Self::check_structure(&manifest, &keys)?;

        let ledger = Self {
            root,
            keys,
            witness,
            inner: Mutex::new(Inner {
                live: LiveTail {
                    generation_id: manifest.active_generation_id.clone(),
                    last_seq: 0,
                    last_tag: String::new(),
                    journal_bytes: 0,
                    open_intents: 0,
                },
                manifest,
                poisoned: None,
                recovery,
                witness_state: WitnessState::Unwitnessed,
            }),
            #[cfg(test)]
            crash_at: None,
        };
        ledger.recover()?;
        Ok(ledger)
    }

    #[cfg(test)]
    pub(crate) fn with_crash_at(mut self, point: CrashPoint) -> Self {
        self.crash_at = Some(point);
        self
    }

    #[cfg(test)]
    pub(crate) fn cut(&self, point: CrashPoint) -> AuditResult<()> {
        if self.crash_at == Some(point) {
            return Err(AuditError::CrashCut);
        }
        Ok(())
    }

    #[cfg(not(test))]
    #[inline]
    pub(crate) fn cut(&self, _point: CrashPoint) -> AuditResult<()> {
        Ok(())
    }

    // ---------------------------------------------------------------- paths

    pub(crate) fn manifest_path(root: &Path) -> PathBuf {
        root.join("manifest.json")
    }

    pub(crate) fn generation_dir(root: &Path, generation_id: &str) -> PathBuf {
        root.join("generations").join(generation_id)
    }

    pub(crate) fn journal_path(root: &Path, generation_id: &str) -> PathBuf {
        Self::generation_dir(root, generation_id).join("journal.jsonl")
    }

    pub(crate) fn anchor_path(root: &Path, generation_id: &str) -> PathBuf {
        Self::generation_dir(root, generation_id).join("anchor.json")
    }

    fn gap_path(&self) -> PathBuf {
        self.root.join("gap.json")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn keys(&self) -> &AuditKeys {
        &self.keys
    }

    // ------------------------------------------------------------ documents

    fn load_manifest(root: &Path, keys: &AuditKeys) -> AuditResult<Option<Manifest>> {
        let path = Self::manifest_path(root);
        if !path.exists() {
            // A temporary is never promoted: it may be a partial write.
            if files::tmp_path(&path)?.exists() {
                return Err(AuditError::Poisoned(PoisonReason::ManifestTmpPresent));
            }
            let generations = root.join("generations");
            let present: Vec<String> = std::fs::read_dir(&generations)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.file_name().to_string_lossy().to_string())
                        .collect()
                })
                .unwrap_or_default();
            if present.is_empty() {
                let _ = std::fs::remove_file(bootstrap_path(root));
                return Ok(None);
            }
            // A first-open legacy import that never reached its manifest commit.
            // Only directories the authenticated marker declared may be cleared,
            // and the v1 source files it copied from are never touched, so this
            // removes a staging copy rather than any evidence.
            let marker_path = bootstrap_path(root);
            if marker_path.exists() {
                let bytes = files::read_bytes(&marker_path)?;
                let marker: BootstrapMarker = serde_json::from_slice(&bytes)
                    .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
                marker.verify(keys)?;
                if present
                    .iter()
                    .all(|name| marker.generation_ids.iter().any(|id| id == name))
                {
                    for name in &present {
                        std::fs::remove_dir_all(generations.join(name)).map_err(|error| {
                            AuditError::Io(format!("clear uncommitted bootstrap: {error}"))
                        })?;
                    }
                    files::fsync_dir(&generations)?;
                    std::fs::remove_file(&marker_path).map_err(|error| {
                        AuditError::Io(format!("clear bootstrap marker: {error}"))
                    })?;
                    files::fsync_dir(root)?;
                    return Ok(None);
                }
            }
            return Err(AuditError::Poisoned(
                PoisonReason::ManifestAbsentWithGenerations,
            ));
        }
        files::reject_symlink(&path)?;
        let bytes = files::read_bytes(&path)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|_| AuditError::Poisoned(PoisonReason::ManifestUnknownSchema))?;
        manifest.verify(keys)?;
        Ok(Some(manifest))
    }

    fn write_manifest(&self, manifest: &mut Manifest) -> AuditResult<()> {
        manifest.manifest_epoch = manifest.manifest_epoch.saturating_add(1);
        manifest.updated_at = Utc::now();
        manifest.seal(&self.keys)?;
        let bytes = serde_json::to_vec(manifest)
            .map_err(|error| AuditError::Io(format!("serialize manifest: {error}")))?;
        files::atomic_write(&Self::manifest_path(&self.root), &bytes)?;
        self.witness.record(&Self::beacon(manifest));
        Ok(())
    }

    fn beacon(manifest: &Manifest) -> WitnessBeacon {
        WitnessBeacon {
            installation_id: manifest.installation_id.clone(),
            manifest_epoch: manifest.manifest_epoch,
            retention_epoch: manifest.retention_epoch,
            global_last_seq_floor: manifest.global_last_seq_floor,
            active_generation_id: manifest.active_generation_id.clone(),
            manifest_mac: manifest.mac.clone(),
        }
    }

    fn load_anchor(&self, generation_id: &str) -> AuditResult<Anchor> {
        let path = Self::anchor_path(&self.root, generation_id);
        files::reject_symlink(&path)?;
        let bytes = files::read_bytes(&path)?;
        let anchor: Anchor = serde_json::from_slice(&bytes)
            .map_err(|_| AuditError::Poisoned(PoisonReason::AnchorMacMismatch))?;
        anchor.verify(&self.keys, generation_id)?;
        Ok(anchor)
    }

    fn write_anchor(&self, live: &LiveTail) -> AuditResult<()> {
        Self::write_anchor_at(
            &self.root,
            &self.keys,
            &live.generation_id,
            live.last_seq,
            &live.last_tag,
            live.journal_bytes,
            live.open_intents,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn write_anchor_at(
        root: &Path,
        keys: &AuditKeys,
        generation_id: &str,
        last_seq: u64,
        last_tag: &str,
        journal_bytes: u64,
        open_intents: u64,
    ) -> AuditResult<()> {
        let mut anchor = Anchor {
            schema: ANCHOR_SCHEMA.to_string(),
            generation_id: generation_id.to_string(),
            key_id: keys.key_id().to_string(),
            last_seq,
            last_tag: last_tag.to_string(),
            journal_bytes,
            open_intents,
            updated_at: Utc::now(),
            mac: String::new(),
        };
        anchor.seal(keys)?;
        let bytes = serde_json::to_vec(&anchor)
            .map_err(|error| AuditError::Io(format!("serialize anchor: {error}")))?;
        files::atomic_write(&Self::anchor_path(root, generation_id), &bytes)
    }

    // --------------------------------------------------------------- init

    /// Build the first committed manifest, optionally importing legacy v1
    /// bytes as sealed, explicitly unauthenticated leading generations.
    ///
    /// Every generation directory is prepared before the manifest is written,
    /// so the manifest rename is the single commit point here exactly as it is
    /// for rotation. An import declares its directories in an authenticated
    /// bootstrap marker first, so a crash before that commit is recoverable
    /// without guessing (see `load_manifest`).
    fn initialize(root: &Path, keys: &AuditKeys, plan: &LegacyImportPlan) -> AuditResult<Manifest> {
        let now = Utc::now();
        let genesis = keys.genesis_tag();
        let total = plan.generations.len() as u32 + 1;
        let ids: Vec<String> = (1..=total).map(generation_id).collect();

        if !plan.generations.is_empty() {
            let marker = BootstrapMarker::new(ids.clone(), keys)?;
            let bytes = serde_json::to_vec(&marker)
                .map_err(|error| AuditError::Io(format!("serialize bootstrap: {error}")))?;
            files::atomic_write(&bootstrap_path(root), &bytes)?;
        }

        let mut generations: Vec<GenerationDescriptor> = Vec::with_capacity(total as usize);
        let mut chain_base = genesis;
        let mut predecessor: Option<String> = None;
        let mut next_first_seq: u64 = 1;

        for (offset, legacy) in plan.generations.iter().enumerate() {
            let id = ids[offset].clone();
            let dir = Self::generation_dir(root, &id);
            files::create_private_dir_all(&dir)?;
            write_imported_journal(&Self::journal_path(root, &id), &legacy.bytes)?;
            let final_tag = keys.import_seal_tag(&id, &legacy.sha256);
            let first_seq = next_first_seq;
            let last_seq = first_seq.saturating_add(legacy.lines).saturating_sub(1);
            Self::write_anchor_at(
                root,
                keys,
                &id,
                last_seq,
                &final_tag,
                legacy.bytes.len() as u64,
                0,
            )?;
            files::fsync_dir(&dir)?;
            generations.push(GenerationDescriptor {
                generation_id: id.clone(),
                index: offset as u32 + 1,
                state: GenerationState::Sealed,
                key_id: keys.key_id().to_string(),
                predecessor_id: predecessor.clone(),
                chain_base: chain_base.clone(),
                first_seq,
                last_seq,
                entry_count: legacy.lines,
                journal_bytes: legacy.bytes.len() as u64,
                journal_sha256: Some(legacy.sha256.clone()),
                final_tag: Some(final_tag.clone()),
                rotation_reason: RotationReason::LegacyImport,
                sequence_origin: SequenceOrigin::ImportAssigned,
                origin_authenticated: false,
                preceding_loss_unknown: legacy.preceding_loss_unknown,
                opened_at: now,
                sealed_at: Some(now),
                tombstoned_at: None,
            });
            chain_base = final_tag;
            predecessor = Some(id);
            next_first_seq = last_seq.saturating_add(1);
        }

        let active_id = ids[total as usize - 1].clone();
        let dir = Self::generation_dir(root, &active_id);
        files::create_private_dir_all(&dir)?;
        let journal = Self::journal_path(root, &active_id);
        if !journal.exists() {
            drop(files::create_private_file_new(&journal)?);
        }
        Self::write_anchor_at(
            root,
            keys,
            &active_id,
            next_first_seq.saturating_sub(1),
            &chain_base,
            0,
            0,
        )?;
        files::fsync_dir(&dir)?;
        generations.push(GenerationDescriptor {
            generation_id: active_id.clone(),
            index: total,
            state: GenerationState::Active,
            key_id: keys.key_id().to_string(),
            predecessor_id: predecessor,
            chain_base,
            first_seq: next_first_seq,
            last_seq: next_first_seq.saturating_sub(1),
            entry_count: 0,
            journal_bytes: 0,
            journal_sha256: None,
            final_tag: None,
            rotation_reason: if plan.generations.is_empty() {
                RotationReason::Genesis
            } else {
                RotationReason::LegacyImport
            },
            sequence_origin: SequenceOrigin::Issued,
            origin_authenticated: true,
            preceding_loss_unknown: false,
            opened_at: now,
            sealed_at: None,
            tombstoned_at: None,
        });

        let mut manifest = Manifest {
            schema: MANIFEST_SCHEMA.to_string(),
            manifest_version: MANIFEST_VERSION,
            installation_id: keys.installation_id().to_string(),
            key_id: keys.key_id().to_string(),
            key_epoch: 1,
            manifest_epoch: 1,
            retention_epoch: 0,
            active_generation_id: active_id,
            global_first_seq: 1,
            global_last_seq_floor: next_first_seq.saturating_sub(1),
            generations,
            tombstones: Vec::new(),
            created_at: now,
            updated_at: now,
            mac: String::new(),
        };
        manifest.seal(keys)?;
        let bytes = serde_json::to_vec(&manifest)
            .map_err(|error| AuditError::Io(format!("serialize manifest: {error}")))?;
        files::atomic_write(&Self::manifest_path(root), &bytes)?;

        if !plan.generations.is_empty() {
            std::fs::remove_file(bootstrap_path(root))
                .map_err(|error| AuditError::Io(format!("clear bootstrap marker: {error}")))?;
            files::fsync_dir(root)?;
        }
        Ok(manifest)
    }

    // --------------------------------------------------------- invariants

    fn check_structure(manifest: &Manifest, keys: &AuditKeys) -> AuditResult<()> {
        if manifest.generations.is_empty() {
            return Err(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid));
        }
        if manifest.installation_id != keys.installation_id() {
            return Err(AuditError::Poisoned(PoisonReason::ManifestMacMismatch));
        }
        let active_count = manifest
            .generations
            .iter()
            .filter(|g| g.state == GenerationState::Active)
            .count();
        if active_count != 1 {
            return Err(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid));
        }
        let last = manifest
            .generations
            .last()
            .ok_or(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid))?;
        if last.state != GenerationState::Active
            || last.generation_id != manifest.active_generation_id
        {
            return Err(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid));
        }
        let genesis = keys.genesis_tag();
        for (position, generation) in manifest.generations.iter().enumerate() {
            if !valid_generation_id(&generation.generation_id) {
                return Err(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid));
            }
            if generation.index as usize != position + 1 {
                return Err(AuditError::Poisoned(
                    PoisonReason::GenerationIndexDiscontinuity,
                ));
            }
            match generation.state {
                GenerationState::Active => {
                    if generation.final_tag.is_some() || generation.journal_sha256.is_some() {
                        return Err(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid));
                    }
                }
                GenerationState::Sealed | GenerationState::Tombstoned => {
                    if generation.final_tag.is_none() || generation.journal_sha256.is_none() {
                        return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
                    }
                }
            }
            if position == 0 {
                if generation.chain_base != genesis || generation.predecessor_id.is_some() {
                    return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
                }
                if generation.first_seq != manifest.global_first_seq {
                    return Err(AuditError::Poisoned(PoisonReason::SequenceDiscontinuity));
                }
            } else {
                let previous = &manifest.generations[position - 1];
                if generation.first_seq != previous.last_seq.saturating_add(1) {
                    return Err(AuditError::Poisoned(PoisonReason::SequenceDiscontinuity));
                }
                let expected_base = previous
                    .final_tag
                    .as_deref()
                    .ok_or(AuditError::Poisoned(PoisonReason::ChainDiscontinuity))?;
                if generation.chain_base != expected_base
                    || generation.predecessor_id.as_deref() != Some(previous.generation_id.as_str())
                {
                    return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
                }
            }
            let tombstoned = manifest
                .tombstones
                .iter()
                .any(|t| t.generation_id == generation.generation_id);
            if tombstoned != (generation.state == GenerationState::Tombstoned) {
                return Err(AuditError::Poisoned(PoisonReason::TombstoneInconsistent));
            }
        }
        let active = manifest.active()?;
        if manifest.global_last_seq_floor != active.last_seq {
            return Err(AuditError::Poisoned(PoisonReason::SequenceDiscontinuity));
        }
        Ok(())
    }

    // ----------------------------------------------------------- recovery

    fn recover(&self) -> AuditResult<()> {
        let mut guard = self.inner.lock();

        // Interrupted retention removal (crash cut T3): the committed manifest
        // is the authorization, so resuming is the only legal deletion path.
        loop {
            let pending = guard
                .manifest
                .tombstones
                .iter()
                .find(|t| {
                    t.removed_at.is_none()
                        && Self::generation_dir(&self.root, &t.generation_id).exists()
                })
                .map(|t| t.generation_id.clone());
            let Some(generation_id) = pending else { break };
            std::fs::remove_dir_all(Self::generation_dir(&self.root, &generation_id))
                .map_err(|error| AuditError::Io(format!("resume removal: {error}")))?;
            files::fsync_dir(&self.root.join("generations"))?;
            if let Some(tombstone) = guard
                .manifest
                .tombstones
                .iter_mut()
                .find(|t| t.generation_id == generation_id)
            {
                tombstone.removed_at = Some(Utc::now());
            }
            let mut manifest = guard.manifest.clone();
            self.write_manifest(&mut manifest)?;
            guard.manifest = manifest;
            guard.recovery.resumed_removals.push(generation_id);
        }
        // Crash cut T4: bytes already gone, only the marker is missing.
        let mut needs_commit = false;
        for tombstone in guard.manifest.tombstones.iter_mut() {
            if tombstone.removed_at.is_none() {
                tombstone.removed_at = Some(Utc::now());
                needs_commit = true;
            }
        }
        if needs_commit {
            let mut manifest = guard.manifest.clone();
            self.write_manifest(&mut manifest)?;
            guard.manifest = manifest;
        }

        // Orphan generation directories (crash cut R2). The manifest is the
        // authority; an orphan is kept for an idempotent retry, never deleted.
        let known: Vec<String> = guard
            .manifest
            .generations
            .iter()
            .map(|g| g.generation_id.clone())
            .collect();
        let next_id = generation_id(guard.manifest.generations.len() as u32 + 1);
        let entries = std::fs::read_dir(self.root.join("generations"))
            .map_err(|error| AuditError::Io(format!("read generations: {error}")))?;
        let mut orphans: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if known.iter().any(|k| k == &name) {
                continue;
            }
            orphans.push(name);
        }
        orphans.sort();
        for name in orphans {
            if name != next_id {
                return Err(AuditError::Poisoned(PoisonReason::OrphanGenerationNotEmpty));
            }
            let journal = Self::journal_path(&self.root, &name);
            let size = std::fs::metadata(&journal).map(|m| m.len()).unwrap_or(0);
            if size != 0 {
                return Err(AuditError::Poisoned(PoisonReason::OrphanGenerationNotEmpty));
            }
            guard.recovery.orphan_generation = Some(name);
        }

        // Durable dropped-entry evidence survives restart.
        if self.gap_path().exists() {
            let bytes = files::read_bytes(&self.gap_path())?;
            let gap: GapFile = serde_json::from_slice(&bytes)
                .map_err(|_| AuditError::Poisoned(PoisonReason::GapMacMismatch))?;
            gap.verify(&self.keys)?;
            guard.recovery.durable_gaps = gap.gaps;
        }

        // Active generation: verify from the chain base and adopt any
        // authenticated tail the anchor does not yet cover.
        let active = guard.manifest.active()?.clone();
        let anchor = self.load_anchor(&active.generation_id)?;
        let scan = self.scan_journal(
            &Self::journal_path(&self.root, &active.generation_id),
            &active.generation_id,
            &active.chain_base,
            active.first_seq,
            active.origin_authenticated,
        )?;
        if scan.last_seq < anchor.last_seq {
            return Err(AuditError::Poisoned(PoisonReason::ActiveJournalTruncated));
        }
        if let Some(evidence) = scan.torn.clone() {
            // The one legal truncate: a byte-exact unterminated trailing run.
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(Self::journal_path(&self.root, &active.generation_id))
                .map_err(|error| AuditError::Io(format!("open for torn-tail trim: {error}")))?;
            file.set_len(scan.complete_len)
                .map_err(|error| AuditError::Io(format!("trim torn tail: {error}")))?;
            file.sync_all()
                .map_err(|error| AuditError::Io(format!("sync torn-tail trim: {error}")))?;
            guard.recovery.torn_tail = Some(evidence);
        }
        let adopted = scan.last_seq.saturating_sub(anchor.last_seq);
        guard.recovery.adopted_tail_entries = adopted;
        guard.live = LiveTail {
            generation_id: active.generation_id.clone(),
            last_seq: scan.last_seq,
            last_tag: scan.last_tag,
            journal_bytes: scan.complete_len,
            open_intents: anchor.open_intents,
        };
        if adopted > 0 || guard.recovery.torn_tail.is_some() {
            let live = guard.live.clone();
            self.write_anchor(&live)?;
        }

        // Rollback witness: fail closed on contradiction, fail soft on outage.
        let verdict = self.witness.check(&Self::beacon(&guard.manifest));
        guard.witness_state = match verdict {
            WitnessVerdict::Rollback { .. } => {
                return Err(AuditError::Poisoned(PoisonReason::RollbackDetected))
            }
            WitnessVerdict::Verified => WitnessState::Verified,
            WitnessVerdict::Unverified(_) => self.witness.state(),
        };

        drop(guard);
        self.close_recovery_evidence()?;
        Ok(())
    }

    /// Append the honest records recovery owes the journal: torn-tail evidence,
    /// unjournaled dropped entries, and uncertain outcomes for open intents.
    fn close_recovery_evidence(&self) -> AuditResult<()> {
        let (torn, open_intents, ungapped) = {
            let guard = self.inner.lock();
            let ungapped: Vec<GapRecord> = guard
                .recovery
                .durable_gaps
                .iter()
                .filter(|gap| !gap.journaled)
                .cloned()
                .collect();
            (
                guard.recovery.torn_tail.clone(),
                guard.live.open_intents,
                ungapped,
            )
        };

        if let Some(evidence) = torn {
            self.append_internal(
                AuditEntryInput::new(
                    "audit.recovery",
                    EntryPhase::Outcome,
                    EntryOutcome::Uncertain,
                )
                .with_reason(EntryReason::RecoveryTornTail),
                Some(evidence),
            )?;
        }
        for gap in ungapped {
            self.append_internal(
                AuditEntryInput::new(
                    "audit.recovery",
                    EntryPhase::Outcome,
                    EntryOutcome::Uncertain,
                )
                .with_reason(EntryReason::RecoveryDroppedEntries),
                Some(RecoveryEvidence {
                    bytes: 0,
                    sha256: String::new(),
                    at_offset: gap.after_seq,
                    lost_entries: gap.lost_entries,
                }),
            )?;
        }
        if !self.inner.lock().recovery.durable_gaps.is_empty() {
            self.mark_gaps_journaled()?;
        }

        if open_intents > 0 {
            // Never fabricate success and never auto-redispatch: the count of
            // interrupted intents is stated exactly, the outcome is uncertain.
            self.append_internal(
                AuditEntryInput::new(
                    "audit.recovery",
                    EntryPhase::Outcome,
                    EntryOutcome::Uncertain,
                )
                .with_reason(EntryReason::HostRestartInterrupted),
                Some(RecoveryEvidence {
                    bytes: 0,
                    sha256: String::new(),
                    at_offset: 0,
                    lost_entries: open_intents,
                }),
            )?;
            let mut guard = self.inner.lock();
            guard.live.open_intents = 0;
            guard.recovery.closed_intents = open_intents;
            let live = guard.live.clone();
            drop(guard);
            self.write_anchor(&live)?;
        }
        Ok(())
    }

    fn mark_gaps_journaled(&self) -> AuditResult<()> {
        let mut guard = self.inner.lock();
        for gap in guard.recovery.durable_gaps.iter_mut() {
            gap.journaled = true;
        }
        let mut file = GapFile::new();
        file.gaps = guard.recovery.durable_gaps.clone();
        drop(guard);
        file.seal(&self.keys)?;
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| AuditError::Io(format!("serialize gap file: {error}")))?;
        files::atomic_write(&self.gap_path(), &bytes)
    }

    // ------------------------------------------------------- journal scan

    fn scan_journal(
        &self,
        path: &Path,
        generation_id: &str,
        chain_base: &str,
        first_seq: u64,
        origin_authenticated: bool,
    ) -> AuditResult<JournalScan> {
        scan_journal_at(
            &self.keys,
            path,
            generation_id,
            chain_base,
            first_seq,
            origin_authenticated,
        )
    }
}

/// Replay and authenticate one journal file. Free-standing on purpose: export
/// re-verification must be able to check a copied tree with a fresh reader that
/// shares no in-memory state with the live ledger.
pub(crate) fn scan_journal_at(
    keys: &AuditKeys,
    path: &Path,
    generation_id: &str,
    chain_base: &str,
    first_seq: u64,
    origin_authenticated: bool,
) -> AuditResult<JournalScan> {
    {
        let bytes = if path.exists() {
            files::reject_symlink(path)?;
            files::read_bytes(path)?
        } else {
            Vec::new()
        };

        if !origin_authenticated {
            // Imported legacy bytes carry no chain. Their *boundary* is
            // authenticated by the import seal; their contents are not, and
            // this code never pretends otherwise.
            return Ok(JournalScan {
                last_seq: first_seq
                    .saturating_add(count_lines(&bytes))
                    .saturating_sub(1),
                last_tag: keys.import_seal_tag(generation_id, &sha256_hex(&bytes)),
                bytes: bytes.len() as u64,
                complete_len: bytes.len() as u64,
                torn: None,
                entry_count: count_lines(&bytes),
            });
        }

        let mut previous = chain_base.to_string();
        let mut expected_seq = first_seq;
        let mut last_seq = first_seq.saturating_sub(1);
        let mut offset: u64 = 0;
        let mut complete_len: u64 = 0;
        let mut entry_count: u64 = 0;

        let mut cursor = 0usize;
        while cursor < bytes.len() {
            let Some(newline) = bytes[cursor..].iter().position(|b| *b == b'\n') else {
                break;
            };
            let line = &bytes[cursor..cursor + newline];
            if newline + 1 > MAX_LINE_BYTES {
                return Err(AuditError::Poisoned(PoisonReason::OversizedLine));
            }
            let record: AuditRecord = serde_json::from_slice(line)
                .map_err(|_| AuditError::Poisoned(PoisonReason::EntryMalformed))?;
            if record.v != RECORD_VERSION {
                return Err(AuditError::Poisoned(PoisonReason::EntryMalformed));
            }
            if record.generation != generation_id {
                return Err(AuditError::Poisoned(PoisonReason::EntryForeignGeneration));
            }
            if record.seq != expected_seq {
                return Err(AuditError::Poisoned(PoisonReason::EntrySequenceBreak));
            }
            if record.prev != previous {
                return Err(AuditError::Poisoned(PoisonReason::ChainDiscontinuity));
            }
            let expected_tag = record.compute_tag(keys)?;
            if !crate::orchestration::constant_time_eq(
                expected_tag.as_bytes(),
                record.tag.as_bytes(),
            ) {
                return Err(AuditError::Poisoned(PoisonReason::EntryMacMismatch));
            }
            previous = record.tag;
            last_seq = record.seq;
            expected_seq = expected_seq.saturating_add(1);
            entry_count = entry_count.saturating_add(1);
            cursor += newline + 1;
            offset = cursor as u64;
            complete_len = offset;
        }

        let torn = if cursor < bytes.len() {
            let trailing = &bytes[cursor..];
            if trailing.len() > MAX_LINE_BYTES {
                return Err(AuditError::Poisoned(PoisonReason::OversizedLine));
            }
            Some(RecoveryEvidence {
                bytes: trailing.len() as u64,
                sha256: sha256_hex(trailing),
                at_offset: offset,
                lost_entries: 1,
            })
        } else {
            None
        };

        Ok(JournalScan {
            last_seq,
            last_tag: previous,
            bytes: bytes.len() as u64,
            complete_len,
            torn,
            entry_count,
        })
    }
}

impl AuditLedger {
    // --------------------------------------------------------------- append

    pub fn append(&self, entry: AuditEntryInput) -> AuditResult<u64> {
        self.append_internal(entry, None)
    }

    fn append_internal(
        &self,
        entry: AuditEntryInput,
        recovery: Option<RecoveryEvidence>,
    ) -> AuditResult<u64> {
        let mut guard = self.inner.lock();
        if let Some(reason) = guard.poisoned {
            return Err(AuditError::Poisoned(reason));
        }
        let live = guard.live.clone();
        let mut record = AuditRecord {
            v: RECORD_VERSION,
            generation: live.generation_id.clone(),
            seq: live.last_seq.saturating_add(1),
            ts: Utc::now(),
            op: bounded(&entry.op, 64),
            phase: entry.phase,
            outcome: entry.outcome,
            reason: entry.reason,
            actor: entry.actor.as_deref().map(|v| self.keys.opaque_digest(v)),
            request: entry.request.as_deref().map(|v| self.keys.opaque_digest(v)),
            scope: entry.scope.as_deref().map(|v| self.keys.opaque_digest(v)),
            authz_rev: entry.authz_rev,
            cap_rev: entry.cap_rev,
            policy_rev: entry.policy_rev,
            recovery,
            prev: live.last_tag.clone(),
            tag: String::new(),
        };
        record.tag = record.compute_tag(&self.keys)?;
        let mut line = serde_json::to_string(&record)
            .map_err(|error| AuditError::Io(format!("serialize record: {error}")))?;
        line.push('\n');
        if line.len() > MAX_LINE_BYTES {
            return Err(AuditError::Refused(RefuseReason::EntryTooLarge));
        }

        let path = Self::journal_path(&self.root, &live.generation_id);
        let written = match files::append_line(&path, &line) {
            Ok(written) => written,
            Err(error) => {
                guard.poisoned = Some(PoisonReason::PartialPersistence);
                return Err(error);
            }
        };
        drop(guard);
        self.cut(CrashPoint::JournalAppendedBeforeAnchor)?;

        let mut guard = self.inner.lock();
        guard.live.last_seq = record.seq;
        guard.live.last_tag = record.tag.clone();
        guard.live.journal_bytes = guard.live.journal_bytes.saturating_add(written);
        guard.live.open_intents = match record.phase {
            EntryPhase::Intent => guard.live.open_intents.saturating_add(1),
            EntryPhase::Outcome => guard.live.open_intents.saturating_sub(1),
        };
        let live = guard.live.clone();
        drop(guard);
        if let Err(error) = self.write_anchor(&live) {
            self.inner.lock().poisoned = Some(PoisonReason::PartialPersistence);
            return Err(error);
        }
        Ok(record.seq)
    }

    /// Record that producer-side entries were dropped before they reached the
    /// journal. Written to the authenticated gap file first, so the evidence
    /// survives even when the journal itself is unwritable.
    pub fn record_dropped(&self, lost_entries: u64) -> AuditResult<()> {
        if lost_entries == 0 {
            return Ok(());
        }
        let (generation_id, after_seq) = {
            let guard = self.inner.lock();
            (guard.live.generation_id.clone(), guard.live.last_seq)
        };
        let mut file = GapFile::new();
        {
            let mut guard = self.inner.lock();
            guard.recovery.durable_gaps.push(GapRecord {
                generation_id,
                after_seq,
                lost_entries,
                reason: EntryReason::RecoveryDroppedEntries,
                recorded_at: Utc::now(),
                journaled: false,
            });
            file.gaps = guard.recovery.durable_gaps.clone();
        }
        file.seal(&self.keys)?;
        let bytes = serde_json::to_vec(&file)
            .map_err(|error| AuditError::Io(format!("serialize gap file: {error}")))?;
        files::atomic_write(&self.gap_path(), &bytes)?;

        self.append_internal(
            AuditEntryInput::new("audit.gap", EntryPhase::Outcome, EntryOutcome::Uncertain)
                .with_reason(EntryReason::RecoveryDroppedEntries),
            Some(RecoveryEvidence {
                bytes: 0,
                sha256: String::new(),
                at_offset: after_seq,
                lost_entries,
            }),
        )?;
        self.mark_gaps_journaled()
    }

    // --------------------------------------------------------------- rotate

    pub fn rotate(&self, reason: RotationReason) -> AuditResult<String> {
        {
            let guard = self.inner.lock();
            if let Some(poison) = guard.poisoned {
                return Err(AuditError::Poisoned(poison));
            }
            if guard.live.open_intents != 0 {
                return Err(AuditError::Refused(RefuseReason::OpenIntentsPresent));
            }
        }

        // R0.5: the sealing record is the last entry of the outgoing generation.
        self.append_internal(
            AuditEntryInput::new(
                "audit.generation.sealing",
                EntryPhase::Outcome,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::GenerationSealing),
            None,
        )?;

        // R1: freeze and verify the outgoing generation end to end.
        let (manifest, live) = {
            let guard = self.inner.lock();
            (guard.manifest.clone(), guard.live.clone())
        };
        let outgoing = manifest.active()?.clone();
        let journal = Self::journal_path(&self.root, &outgoing.generation_id);
        let scan = self.scan_journal(
            &journal,
            &outgoing.generation_id,
            &outgoing.chain_base,
            outgoing.first_seq,
            outgoing.origin_authenticated,
        )?;
        if scan.torn.is_some() || scan.last_seq != live.last_seq {
            return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
        }
        let journal_bytes = files::read_bytes(&journal).unwrap_or_default();
        let journal_sha256 = sha256_hex(&journal_bytes);
        self.cut(CrashPoint::R1Frozen)?;

        // R2: prepare the next generation on disk. No manifest change yet.
        let next_index = outgoing.index.saturating_add(1);
        let next_id = generation_id(next_index);
        let next_dir = Self::generation_dir(&self.root, &next_id);
        if !next_dir.exists() {
            files::create_private_dir_new(&next_dir)?;
        }
        let next_journal = Self::journal_path(&self.root, &next_id);
        if !next_journal.exists() {
            drop(files::create_private_file_new(&next_journal)?);
        }
        let next_live = LiveTail {
            generation_id: next_id.clone(),
            last_seq: scan.last_seq,
            last_tag: scan.last_tag.clone(),
            journal_bytes: 0,
            open_intents: 0,
        };
        self.write_anchor(&next_live)?;
        files::fsync_dir(&next_dir)?;
        files::fsync_dir(&self.root.join("generations"))?;
        self.cut(CrashPoint::R2Prepared)?;

        // R3: the single commit point.
        let now = Utc::now();
        let mut manifest = manifest;
        {
            let descriptor = manifest
                .generation_mut(&outgoing.generation_id)
                .ok_or(AuditError::Poisoned(PoisonReason::ActiveGenerationInvalid))?;
            descriptor.state = GenerationState::Sealed;
            descriptor.last_seq = scan.last_seq;
            descriptor.entry_count = scan.entry_count;
            descriptor.journal_bytes = scan.bytes;
            descriptor.journal_sha256 = Some(journal_sha256);
            descriptor.final_tag = Some(scan.last_tag.clone());
            descriptor.sealed_at = Some(now);
        }
        manifest.generations.push(GenerationDescriptor {
            generation_id: next_id.clone(),
            index: next_index,
            state: GenerationState::Active,
            key_id: self.keys.key_id().to_string(),
            predecessor_id: Some(outgoing.generation_id.clone()),
            chain_base: scan.last_tag.clone(),
            first_seq: scan.last_seq.saturating_add(1),
            last_seq: scan.last_seq,
            entry_count: 0,
            journal_bytes: 0,
            journal_sha256: None,
            final_tag: None,
            rotation_reason: reason,
            sequence_origin: SequenceOrigin::Issued,
            origin_authenticated: true,
            preceding_loss_unknown: false,
            opened_at: now,
            sealed_at: None,
            tombstoned_at: None,
        });
        manifest.active_generation_id = next_id.clone();
        manifest.global_last_seq_floor = scan.last_seq;
        self.write_manifest(&mut manifest)?;
        {
            let mut guard = self.inner.lock();
            guard.manifest = manifest;
            guard.live = next_live;
        }
        self.cut(CrashPoint::R3Committed)?;

        // R4: publish.
        self.append_internal(
            AuditEntryInput::new(
                "audit.generation.opened",
                EntryPhase::Outcome,
                EntryOutcome::Accepted,
            )
            .with_reason(EntryReason::GenerationOpened),
            None,
        )?;
        Ok(next_id)
    }

    // -------------------------------------------------------------- verify

    pub fn verify_generation(&self, generation_id: &str) -> AuditResult<GenerationVerification> {
        let manifest = self.inner.lock().manifest.clone();
        let descriptor = manifest
            .generation(generation_id)
            .ok_or(AuditError::Refused(RefuseReason::GenerationUnknown))?
            .clone();
        if descriptor.state == GenerationState::Tombstoned {
            return Err(AuditError::Refused(RefuseReason::GenerationTombstoned));
        }
        let path = Self::journal_path(&self.root, generation_id);
        let scan = self.scan_journal(
            &path,
            generation_id,
            &descriptor.chain_base,
            descriptor.first_seq,
            descriptor.origin_authenticated,
        )?;
        if scan.torn.is_some() && descriptor.state != GenerationState::Active {
            return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
        }
        let bytes = files::read_bytes(&path).unwrap_or_default();
        let journal_sha256 = sha256_hex(&bytes);
        if descriptor.state == GenerationState::Sealed {
            if scan.last_seq != descriptor.last_seq {
                return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
            }
            if descriptor.journal_sha256.as_deref() != Some(journal_sha256.as_str()) {
                return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
            }
            if descriptor.final_tag.as_deref() != Some(scan.last_tag.as_str()) {
                return Err(AuditError::Poisoned(PoisonReason::SealedGenerationChanged));
            }
        }
        Ok(GenerationVerification {
            generation_id: generation_id.to_string(),
            first_seq: descriptor.first_seq,
            last_seq: scan.last_seq,
            entry_count: scan.entry_count,
            journal_bytes: scan.bytes,
            journal_sha256,
            final_tag: scan.last_tag,
            origin_authenticated: descriptor.origin_authenticated,
        })
    }

    pub fn verify_all(&self) -> AuditResult<Vec<GenerationVerification>> {
        let manifest = self.inner.lock().manifest.clone();
        let mut out = Vec::new();
        for descriptor in &manifest.generations {
            if descriptor.state == GenerationState::Tombstoned {
                continue;
            }
            out.push(self.verify_generation(&descriptor.generation_id)?);
        }
        Ok(out)
    }

    // -------------------------------------------------------------- status

    pub fn status(&self) -> AuditStatus {
        let guard = self.inner.lock();
        AuditStatus {
            installation_id: guard.manifest.installation_id.clone(),
            key_id: guard.manifest.key_id.clone(),
            manifest_epoch: guard.manifest.manifest_epoch,
            retention_epoch: guard.manifest.retention_epoch,
            active_generation_id: guard.manifest.active_generation_id.clone(),
            global_first_seq: guard.manifest.global_first_seq,
            global_last_seq: guard.live.last_seq,
            generations: guard.manifest.generations.len(),
            tombstones: guard.manifest.tombstones.len(),
            open_intents: guard.live.open_intents,
            journal_bytes: guard.live.journal_bytes,
            poisoned: guard.poisoned,
            witness_state: guard.witness_state,
            imported_generations: guard
                .manifest
                .generations
                .iter()
                .filter(|g| !g.origin_authenticated)
                .count(),
            recovery: guard.recovery.clone(),
        }
    }

    pub(crate) fn manifest_snapshot(&self) -> Manifest {
        self.inner.lock().manifest.clone()
    }

    pub(crate) fn open_intents(&self) -> u64 {
        self.inner.lock().live.open_intents
    }

    pub(crate) fn is_poisoned(&self) -> Option<PoisonReason> {
        self.inner.lock().poisoned
    }

    pub(crate) fn witness_state(&self) -> WitnessState {
        self.inner.lock().witness_state
    }

    pub(crate) fn commit_manifest(&self, manifest: Manifest) -> AuditResult<()> {
        let mut manifest = manifest;
        self.write_manifest(&mut manifest)?;
        self.inner.lock().manifest = manifest;
        Ok(())
    }
}

fn count_lines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|b| **b == b'\n').count() as u64
}

fn bounded(value: &str, max: usize) -> String {
    crate::textutil::truncate_at_char_boundary(value, max).to_string()
}
