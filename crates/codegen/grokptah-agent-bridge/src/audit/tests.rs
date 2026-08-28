//! Adversarial and property battery for `grokptah-audit.v2` (#443).
//!
//! Cases 1-35 are the ported reference-model battery from the design review:
//! continuity, tamper, rotation crash cuts, retention, export, rollback, and
//! lifecycle. Cases 36-45 add legacy migration, backward reads, durable
//! dropped-entry evidence, and private-file assertions that a filesystem model
//! could not express.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tempfile::TempDir;

use super::documents::*;
use super::export::{verify_export, ExportFormat};
use super::ledger::{AuditEntryInput, AuditLedger, AuditLedgerOptions, CrashPoint};
use super::retention::RetentionRequest;
use super::witness::{AuditWitness, WitnessBeacon, WitnessState, WitnessVerdict};
use super::{AuditError, AuditKeys, AuditResult, PoisonReason, RefuseReason};

// ------------------------------------------------------------------ helpers

fn keys() -> Arc<AuditKeys> {
    Arc::new(AuditKeys::derive(b"grokptah-audit-test-installation"))
}

fn foreign_keys() -> Arc<AuditKeys> {
    Arc::new(AuditKeys::derive(b"a-different-installation-entirely"))
}

fn entry(op: &str) -> AuditEntryInput {
    AuditEntryInput::new(op, EntryPhase::Outcome, EntryOutcome::Accepted)
}

fn open(root: &Path) -> AuditResult<AuditLedger> {
    AuditLedger::open(root, keys())
}

fn opened(root: &Path) -> AuditLedger {
    open(root).expect("ledger opens")
}

/// Five committed entries in generation 1.
fn fresh(root: &Path) -> AuditLedger {
    let ledger = opened(root);
    for index in 0..5 {
        ledger
            .append(entry(&format!("op.{index}")))
            .expect("append");
    }
    ledger
}

fn journal_of(root: &Path, generation: &str) -> PathBuf {
    AuditLedger::journal_path(root, generation)
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| line.to_string())
        .collect()
}

fn write_lines(path: &Path, lines: &[String]) {
    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    std::fs::write(path, body).expect("rewrite journal");
}

fn poison_of(error: AuditError) -> PoisonReason {
    error
        .poison_reason()
        .unwrap_or_else(|| panic!("expected a poison, got {error}"))
}

fn refusal_of(error: AuditError) -> RefuseReason {
    match error {
        AuditError::Refused(reason) => reason,
        other => panic!("expected a refusal, got {other}"),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create snapshot dir");
    for entry in std::fs::read_dir(from).expect("read dir").flatten() {
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}

/// Rotate with a crash injected at `point`, then reopen from disk.
fn crash_rotate(root: &Path, point: CrashPoint) -> AuditLedger {
    let ledger = fresh(root).with_crash_at(point);
    let result = ledger.rotate(RotationReason::Operator);
    assert!(
        result.is_err(),
        "crash cut {point:?} must interrupt rotation"
    );
    drop(ledger);
    opened(root)
}

/// Max-epoch witness. Fail-closed on contradiction, fail-soft on outage.
#[derive(Default)]
struct MaxEpochWitness {
    max: Mutex<u64>,
    online: AtomicBool,
}

impl MaxEpochWitness {
    fn online() -> Arc<Self> {
        let witness = Arc::new(Self::default());
        witness.online.store(true, Ordering::SeqCst);
        witness
    }

    fn take_offline(&self) {
        self.online.store(false, Ordering::SeqCst);
    }
}

impl AuditWitness for MaxEpochWitness {
    fn record(&self, beacon: &WitnessBeacon) {
        if !self.online.load(Ordering::SeqCst) {
            return;
        }
        let mut max = self.max.lock();
        *max = (*max).max(beacon.manifest_epoch);
    }

    fn check(&self, beacon: &WitnessBeacon) -> WitnessVerdict {
        if !self.online.load(Ordering::SeqCst) {
            return WitnessVerdict::Unverified("witness offline");
        }
        let mut max = self.max.lock();
        if beacon.manifest_epoch < *max {
            return WitnessVerdict::Rollback {
                local: beacon.manifest_epoch,
                witness: *max,
            };
        }
        *max = beacon.manifest_epoch;
        WitnessVerdict::Verified
    }

    fn state(&self) -> WitnessState {
        if self.online.load(Ordering::SeqCst) {
            WitnessState::Verified
        } else {
            WitnessState::Unverified
        }
    }
}

// ------------------------------------------------------------- continuity

#[test]
fn seq_is_global_and_never_resets_across_rotation() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    for index in 0..3 {
        ledger.append(entry(&format!("post.{index}"))).unwrap();
    }
    let manifest = ledger.manifest_snapshot();
    let first = &manifest.generations[0];
    let second = &manifest.generations[1];
    // 5 entries + the sealing entry close generation 1 at seq 6.
    assert_eq!(first.last_seq, 6);
    assert_eq!(second.first_seq, 7, "sequence must continue, never reset");
    assert_eq!(second.chain_base, first.final_tag.clone().unwrap());
    // 7 = generation.opened, then three more.
    assert_eq!(ledger.status().global_last_seq, 10);
}

#[test]
fn chain_is_continuous_across_generations() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("after")).unwrap();
    let manifest = ledger.manifest_snapshot();
    let first = &manifest.generations[0];
    let second = &manifest.generations[1];
    assert_eq!(second.chain_base, first.final_tag.clone().unwrap());
    let verification = ledger.verify_generation(&second.generation_id).unwrap();
    assert_eq!(verification.last_seq, ledger.status().global_last_seq);
}

#[test]
fn renumbering_an_entry_is_rejected() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    drop(ledger);

    // The sequence is inside the MAC input, so the tag alone changes with it.
    let path = journal_of(dir.path(), &generation);
    let lines = read_lines(&path);
    let mut record: AuditRecord = serde_json::from_str(&lines[2]).unwrap();
    let original_tag = record.tag.clone();
    record.seq += 40;
    let renumbered_tag = record.compute_tag(&keys()).unwrap();
    assert_ne!(
        original_tag, renumbered_tag,
        "seq must be authenticated by the entry tag"
    );

    // And the on-disk renumbering is rejected at open.
    let mut lines = lines;
    lines[2] = serde_json::to_string(&record).unwrap();
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::EntrySequenceBreak
    );
}

#[test]
fn moving_an_entry_into_another_generation_is_rejected() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let manifest = ledger.manifest_snapshot();
    let first = manifest.generations[0].generation_id.clone();
    let second = manifest.generations[1].generation_id.clone();
    drop(ledger);

    let stolen = read_lines(&journal_of(dir.path(), &first))[0].clone();
    let mut record: AuditRecord = serde_json::from_str(&stolen).unwrap();
    let original_tag = record.tag.clone();
    record.generation = second.clone();
    assert_ne!(
        original_tag,
        record.compute_tag(&keys()).unwrap(),
        "the generation id must be authenticated by the entry tag"
    );

    let path = journal_of(dir.path(), &second);
    let mut lines = read_lines(&path);
    lines.push(stolen);
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::EntryForeignGeneration
    );
}

#[test]
fn manifest_sequence_discontinuity_is_rejected_at_open() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let mut manifest = ledger.manifest_snapshot();
    drop(ledger);

    // Re-sealed with the correct key, so only the structural rule can catch it.
    manifest.generations[1].first_seq += 1;
    manifest.seal(&keys()).unwrap();
    std::fs::write(
        AuditLedger::manifest_path(dir.path()),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::SequenceDiscontinuity
    );
}

// ----------------------------------------------------- tamper / truncation

#[test]
fn in_place_entry_tamper_is_rejected() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    drop(ledger);

    let path = journal_of(dir.path(), &generation);
    let mut lines = read_lines(&path);
    let mut record: AuditRecord = serde_json::from_str(&lines[1]).unwrap();
    record.outcome = EntryOutcome::Rejected;
    lines[1] = serde_json::to_string(&record).unwrap();
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::EntryMacMismatch
    );
}

#[test]
fn reorder_of_two_entries_is_rejected() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    drop(ledger);

    let path = journal_of(dir.path(), &generation);
    let mut lines = read_lines(&path);
    lines.swap(1, 2);
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::EntrySequenceBreak
    );
}

#[test]
fn silent_tail_truncation_of_the_active_generation_is_rejected() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    drop(ledger);

    // Drop a complete, anchored line: the anchor is a floor, so this is loss.
    let path = journal_of(dir.path(), &generation);
    let mut lines = read_lines(&path);
    lines.pop();
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::ActiveJournalTruncated
    );
}

#[test]
fn torn_unterminated_write_is_recovered_and_recorded() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    drop(ledger);

    let path = journal_of(dir.path(), &generation);
    let torn = b"{\"v\":2,\"gen\":\"g-0000";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(torn).unwrap();
    file.sync_all().unwrap();

    let ledger = opened(dir.path());
    let status = ledger.status();
    let evidence = status
        .recovery
        .torn_tail
        .expect("torn tail must be surfaced, never silently dropped");
    assert_eq!(evidence.bytes, torn.len() as u64, "byte-exact evidence");
    assert_eq!(evidence.sha256.len(), 64);
    // The five committed entries survive and the recovery record is appended.
    assert_eq!(status.global_last_seq, 6);
    let records: Vec<AuditRecord> = read_lines(&path)
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[5].reason, Some(EntryReason::RecoveryTornTail));
    assert_eq!(records[5].outcome, EntryOutcome::Uncertain);
}

#[test]
fn sealed_generation_length_change_poisons() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let sealed = ledger.manifest_snapshot().generations[0]
        .generation_id
        .clone();

    let path = journal_of(dir.path(), &sealed);
    let mut lines = read_lines(&path);
    lines.pop();
    write_lines(&path, &lines);
    assert_eq!(
        poison_of(ledger.verify_generation(&sealed).unwrap_err()),
        PoisonReason::SealedGenerationChanged
    );
}

#[test]
fn wrong_installation_key_is_rejected() {
    let dir = TempDir::new().unwrap();
    drop(fresh(dir.path()));
    let error = AuditLedger::open(dir.path(), foreign_keys()).unwrap_err();
    assert_eq!(poison_of(error), PoisonReason::ManifestMacMismatch);
}

#[test]
fn manifest_tmp_is_never_promoted() {
    let dir = TempDir::new().unwrap();
    drop(fresh(dir.path()));
    let manifest = AuditLedger::manifest_path(dir.path());
    let tmp = manifest.with_file_name("manifest.json.tmp");
    std::fs::copy(&manifest, &tmp).unwrap();
    std::fs::remove_file(&manifest).unwrap();
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::ManifestTmpPresent
    );
}

// ------------------------------------------------------ rotation crash cuts

#[test]
fn crash_cut_r1_reopens_the_old_generation() {
    let dir = TempDir::new().unwrap();
    let ledger = crash_rotate(dir.path(), CrashPoint::R1Frozen);
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.active_generation_id, "g-000001");
    assert_eq!(manifest.generations.len(), 1);
    // The retry succeeds: the freeze was in memory only.
    assert_eq!(ledger.rotate(RotationReason::Operator).unwrap(), "g-000002");
}

#[test]
fn crash_cut_r2_reopens_old_and_keeps_the_orphan() {
    let dir = TempDir::new().unwrap();
    let ledger = crash_rotate(dir.path(), CrashPoint::R2Prepared);
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.active_generation_id, "g-000001");
    assert!(
        AuditLedger::generation_dir(dir.path(), "g-000002").exists(),
        "an orphan generation is kept for an idempotent retry, never deleted"
    );
    assert_eq!(
        ledger.status().recovery.orphan_generation.as_deref(),
        Some("g-000002")
    );
    assert_eq!(ledger.rotate(RotationReason::Operator).unwrap(), "g-000002");
}

#[test]
fn crash_cut_r2_with_a_non_empty_orphan_poisons() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path()).with_crash_at(CrashPoint::R2Prepared);
    assert!(ledger.rotate(RotationReason::Operator).is_err());
    drop(ledger);
    // Unreachable before the commit, so it means tampering: never guess.
    std::fs::write(journal_of(dir.path(), "g-000002"), b"{}\n").unwrap();
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::OrphanGenerationNotEmpty
    );
}

#[test]
fn crash_cut_r3_reopens_the_new_generation() {
    let dir = TempDir::new().unwrap();
    let ledger = crash_rotate(dir.path(), CrashPoint::R3Committed);
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.active_generation_id, "g-000002");
    assert_eq!(manifest.generations[0].state, GenerationState::Sealed);
    // Continues from the sealed generation; it does not reset.
    assert_eq!(ledger.status().global_last_seq, 6);
    ledger.append(entry("after")).unwrap();
    assert_eq!(ledger.status().global_last_seq, 7);
}

#[test]
fn crash_between_append_and_anchor_adopts_the_authenticated_tail() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path()).with_crash_at(CrashPoint::JournalAppendedBeforeAnchor);
    assert!(ledger.append(entry("orphan-line")).is_err());
    drop(ledger);

    let ledger = opened(dir.path());
    assert_eq!(
        ledger.status().recovery.adopted_tail_entries,
        1,
        "an authenticated tail beyond the anchor is adopted, not discarded"
    );
    assert_eq!(ledger.status().global_last_seq, 6);
}

#[test]
fn rotation_is_refused_while_an_intent_is_open() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger
        .append(AuditEntryInput::new(
            "submit",
            EntryPhase::Intent,
            EntryOutcome::Accepted,
        ))
        .unwrap();
    assert_eq!(
        refusal_of(ledger.rotate(RotationReason::Bytes).unwrap_err()),
        RefuseReason::OpenIntentsPresent
    );
}

// ---------------------------------------------------------------- retention

#[test]
fn retention_refuses_the_active_generation() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let error = ledger
        .retain(RetentionRequest::new("g-000001").with_export_seal("seal-x"))
        .unwrap_err();
    assert_eq!(refusal_of(error), RefuseReason::GenerationIsActive);
}

#[test]
fn retention_refuses_an_unexported_generation_without_an_override() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::new("g-000001"))
                .unwrap_err()
        ),
        RefuseReason::GenerationUnexported
    );
    // The override is allowed, and is recorded permanently in the tombstone.
    ledger
        .retain(RetentionRequest::new("g-000001").allow_unexported())
        .unwrap();
    assert!(ledger.manifest_snapshot().tombstones[0].allow_unexported);
}

#[test]
fn tombstone_first_retention_keeps_the_chain_across_the_hole() {
    let dir = TempDir::new().unwrap();
    let export_dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    let receipt = ledger
        .export(&export_dir.path().join("out"), ExportFormat::Auto)
        .unwrap();
    ledger
        .retain(RetentionRequest::new("g-000001").with_export_seal(&receipt.seal_id))
        .unwrap();
    drop(ledger);

    let ledger = opened(dir.path());
    let manifest = ledger.manifest_snapshot();
    let tombstone = &manifest.tombstones[0];
    let survivor = &manifest.generations[1];
    assert_eq!(manifest.generations[0].state, GenerationState::Tombstoned);
    assert_eq!(
        survivor.first_seq,
        tombstone.last_seq + 1,
        "the hole must stay sequence-contiguous"
    );
    assert_eq!(
        survivor.chain_base, tombstone.final_tag,
        "the chain must stitch across the hole"
    );
    assert!(!AuditLedger::generation_dir(dir.path(), "g-000001").exists());
    assert_eq!(manifest.retention_epoch, 1);
}

#[test]
fn crash_cut_t3_resumes_removal_at_open() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    let ledger = ledger.with_crash_at(CrashPoint::T3Committed);
    assert!(ledger
        .retain(RetentionRequest::new("g-000001").allow_unexported())
        .is_err());
    assert!(
        AuditLedger::generation_dir(dir.path(), "g-000001").exists(),
        "the tombstone commits before the bytes are removed"
    );
    drop(ledger);

    let ledger = opened(dir.path());
    assert!(
        !AuditLedger::generation_dir(dir.path(), "g-000001").exists(),
        "a committed tombstone authorizes resuming the removal"
    );
    assert_eq!(
        ledger.status().recovery.resumed_removals,
        vec!["g-000001".to_string()]
    );
    assert!(ledger.manifest_snapshot().tombstones[0]
        .removed_at
        .is_some());
}

#[test]
fn crash_cut_t4_converges_without_loss() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    let ledger = ledger.with_crash_at(CrashPoint::T4Removed);
    assert!(ledger
        .retain(RetentionRequest::new("g-000001").allow_unexported())
        .is_err());
    drop(ledger);

    let ledger = opened(dir.path());
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.tombstones[0].generation_id, "g-000001");
    assert!(manifest.tombstones[0].removed_at.is_some());
    assert_eq!(
        manifest.generations[1].chain_base,
        manifest.tombstones[0].final_tag
    );
}

#[test]
fn retention_refuses_a_sealed_generation_whose_bytes_changed() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_of(dir.path(), "g-000001"))
        .unwrap();
    file.write_all(b"{}\n").unwrap();
    file.sync_all().unwrap();

    let error = ledger
        .retain(RetentionRequest::new("g-000001").allow_unexported())
        .unwrap_err();
    assert_eq!(poison_of(error), PoisonReason::SealedGenerationChanged);
    assert!(
        AuditLedger::generation_dir(dir.path(), "g-000001").exists(),
        "a refused retention never removes bytes"
    );
}

// ------------------------------------------------------------------- export

#[test]
fn never_rotated_ledger_exports_as_v1_and_v2() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let v1 = ledger
        .export(&out.path().join("v1"), ExportFormat::V1)
        .unwrap();
    let v2 = ledger
        .export(&out.path().join("v2"), ExportFormat::V2)
        .unwrap();
    assert!(v1.schema.ends_with(".v1") && v1.complete);
    assert!(v2.schema.ends_with(".v2") && v2.complete);
    assert_eq!(v1.global_last_seq, 5);
    // Auto picks v1 for a never-rotated, fully authenticated ledger.
    let auto = ledger
        .export(&out.path().join("auto"), ExportFormat::Auto)
        .unwrap();
    assert!(auto.schema.ends_with(".v1"));
}

#[test]
fn multi_generation_refuses_v1_and_auto_selects_v2() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    assert_eq!(
        refusal_of(
            ledger
                .export(&out.path().join("v1"), ExportFormat::V1)
                .unwrap_err()
        ),
        RefuseReason::ExportV1IncompatibleMultiGeneration
    );
    let auto = ledger
        .export(&out.path().join("auto"), ExportFormat::Auto)
        .unwrap();
    assert!(auto.schema.ends_with(".v2"));
    assert!(auto.complete);
    assert_eq!(auto.generations_exported, 2);
}

#[test]
fn export_after_retention_is_incomplete_with_an_explicit_hole() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    ledger
        .retain(RetentionRequest::new("g-000001").allow_unexported())
        .unwrap();

    let dest = out.path().join("after");
    let receipt = ledger.export(&dest, ExportFormat::Auto).unwrap();
    assert!(
        !receipt.complete,
        "a retained hole must never be reported as complete"
    );
    assert_eq!(receipt.holes, 1);
    assert_eq!(receipt.generations_exported, 1);

    // The independent verifier agrees, and coverage tiles the range exactly.
    let verified = verify_export(&dest, &keys()).unwrap();
    assert_eq!(verified.holes, 1);
    assert!(!verified.complete);
    assert_eq!(verified.global_first_seq, 1);
}

#[test]
fn export_never_rotates_truncates_or_deletes() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let path = journal_of(dir.path(), "g-000001");
    let before = std::fs::read(&path).unwrap();
    let epoch = ledger.manifest_snapshot().manifest_epoch;

    ledger
        .export(&out.path().join("out"), ExportFormat::Auto)
        .unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(ledger.manifest_snapshot().manifest_epoch, epoch);
    assert_eq!(ledger.manifest_snapshot().generations.len(), 1);
}

#[test]
fn export_into_an_existing_directory_is_refused() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let dest = out.path().join("occupied");
    std::fs::create_dir_all(&dest).unwrap();
    assert_eq!(
        refusal_of(ledger.export(&dest, ExportFormat::Auto).unwrap_err()),
        RefuseReason::ExportDestinationExists
    );
}

// ----------------------------------------------------------------- rollback

#[test]
fn joint_rollback_is_undetected_without_a_witness() {
    let dir = TempDir::new().unwrap();
    let snapshot = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("before-snapshot")).unwrap();
    drop(ledger);

    copy_tree(dir.path(), &snapshot.path().join("snap"));
    let ledger = opened(dir.path());
    ledger.append(entry("after-snapshot")).unwrap();
    drop(ledger);

    std::fs::remove_dir_all(dir.path()).unwrap();
    copy_tree(&snapshot.path().join("snap"), dir.path());

    // This is the honest limit of local-file integrity: a coherent earlier
    // snapshot satisfies every invariant, so the ledger opens clean.
    let ledger = opened(dir.path());
    assert!(ledger.status().poisoned.is_none());
    assert_eq!(ledger.status().witness_state, WitnessState::Unwitnessed);
}

#[test]
fn joint_rollback_is_detected_by_a_witness() {
    let dir = TempDir::new().unwrap();
    let snapshot = TempDir::new().unwrap();
    let witness = MaxEpochWitness::online();

    let ledger = AuditLedger::open_with_witness(
        dir.path(),
        keys(),
        witness.clone() as Arc<dyn AuditWitness>,
    )
    .unwrap();
    for index in 0..3 {
        ledger.append(entry(&format!("op.{index}"))).unwrap();
    }
    ledger.rotate(RotationReason::Bytes).unwrap();
    drop(ledger);

    copy_tree(dir.path(), &snapshot.path().join("snap"));
    let ledger = AuditLedger::open_with_witness(
        dir.path(),
        keys(),
        witness.clone() as Arc<dyn AuditWitness>,
    )
    .unwrap();
    ledger.rotate(RotationReason::Bytes).unwrap();
    drop(ledger);

    std::fs::remove_dir_all(dir.path()).unwrap();
    copy_tree(&snapshot.path().join("snap"), dir.path());

    let error = AuditLedger::open_with_witness(
        dir.path(),
        keys(),
        witness.clone() as Arc<dyn AuditWitness>,
    )
    .unwrap_err();
    assert_eq!(poison_of(error), PoisonReason::RollbackDetected);
}

#[test]
fn witness_outage_is_fail_soft_and_receipt_honest() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let witness = MaxEpochWitness::online();
    let ledger = AuditLedger::open_with_witness(
        dir.path(),
        keys(),
        witness.clone() as Arc<dyn AuditWitness>,
    )
    .unwrap();
    ledger.append(entry("first")).unwrap();
    drop(ledger);

    witness.take_offline();
    let ledger = AuditLedger::open_with_witness(
        dir.path(),
        keys(),
        witness.clone() as Arc<dyn AuditWitness>,
    )
    .unwrap();
    // An unreachable witness must not take the host down...
    assert!(ledger.status().poisoned.is_none());
    // ...and must not silently upgrade into an implied guarantee.
    assert_eq!(ledger.status().witness_state, WitnessState::Unverified);
    let receipt = ledger
        .export(&out.path().join("out"), ExportFormat::Auto)
        .unwrap();
    assert_eq!(receipt.witness_state, WitnessState::Unverified);
}

// ---------------------------------------------------------------- lifecycle

#[test]
fn fresh_install_initializes_generation_one_from_genesis() {
    let dir = TempDir::new().unwrap();
    let ledger = opened(dir.path());
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.active_generation_id, "g-000001");
    assert_eq!(manifest.global_first_seq, 1);
    assert_eq!(manifest.generations[0].chain_base, keys().genesis_tag());
    assert!(ledger.status().recovery.initialized);
}

#[test]
fn repeated_open_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let epoch = ledger.manifest_snapshot().manifest_epoch;
    let last_seq = ledger.status().global_last_seq;
    drop(ledger);

    for _ in 0..3 {
        let ledger = opened(dir.path());
        assert_eq!(ledger.manifest_snapshot().manifest_epoch, epoch);
        assert_eq!(ledger.status().global_last_seq, last_seq);
    }
}

#[test]
fn one_hundred_rotations_keep_sequence_exact_with_zero_gaps() {
    let dir = TempDir::new().unwrap();
    let ledger = opened(dir.path());
    for round in 0..100 {
        for index in 0..3 {
            ledger.append(entry(&format!("r{round}.{index}"))).unwrap();
        }
        ledger.rotate(RotationReason::Bytes).unwrap();
    }
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.generations.len(), 101);
    let mut expected = 1u64;
    for generation in &manifest.generations {
        assert_eq!(
            generation.first_seq, expected,
            "{} broke sequence continuity",
            generation.generation_id
        );
        expected = generation.last_seq + 1;
    }
    // 100 rounds x (3 entries + sealing + opened) = 500.
    assert_eq!(ledger.status().global_last_seq, 500);
}

// ------------------------------------------ migration, backward read, gaps

/// Write a legacy v1 audit directory exactly as `orchestration/store.rs` leaves
/// it: `audit.jsonl` current, `audit.jsonl.1` the one rotated predecessor.
fn legacy_v1_dir(base: &Path, older: &str, current: &str) -> PathBuf {
    let dir = base.join("legacy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("audit.jsonl.1"), older).unwrap();
    std::fs::write(dir.join("audit.jsonl"), current).unwrap();
    dir
}

fn open_with_legacy(root: &Path, legacy: &Path) -> AuditResult<AuditLedger> {
    AuditLedger::open_with_options(
        root,
        keys(),
        AuditLedgerOptions {
            legacy_v1_dir: Some(legacy.to_path_buf()),
            ..AuditLedgerOptions::default()
        },
    )
}

#[test]
fn legacy_v1_bytes_are_imported_verbatim() {
    let dir = TempDir::new().unwrap();
    let older = "{\"tool\":\"auth\",\"outcome\":\"rejected\"}\n";
    let current = "{\"tool\":\"ptah_submit_task\",\"outcome\":\"accepted\"}\n\
                   {\"tool\":\"ptah_cancel\",\"outcome\":\"accepted\"}\n";
    let legacy = legacy_v1_dir(dir.path(), older, current);
    let ledger = open_with_legacy(&dir.path().join("audit"), &legacy).unwrap();

    let root = dir.path().join("audit");
    assert_eq!(
        std::fs::read_to_string(journal_of(&root, "g-000001")).unwrap(),
        older,
        "imported bytes must be preserved byte for byte"
    );
    assert_eq!(
        std::fs::read_to_string(journal_of(&root, "g-000002")).unwrap(),
        current
    );
    // The v1 source files are inputs only and are never moved or truncated.
    assert_eq!(
        std::fs::read_to_string(legacy.join("audit.jsonl")).unwrap(),
        current
    );
    assert_eq!(ledger.status().imported_generations, 2);
}

#[test]
fn imported_generation_is_marked_unauthenticated_with_preceding_loss() {
    let dir = TempDir::new().unwrap();
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");
    let ledger = open_with_legacy(&dir.path().join("audit"), &legacy).unwrap();
    let manifest = ledger.manifest_snapshot();

    let oldest = &manifest.generations[0];
    assert!(
        !oldest.origin_authenticated,
        "legacy bytes are not vouched for"
    );
    assert_eq!(oldest.sequence_origin, SequenceOrigin::ImportAssigned);
    assert!(
        oldest.preceding_loss_unknown,
        "v1 already destroyed anything older, and that must be stated"
    );
    assert_eq!(oldest.first_seq, 1);
    assert_eq!(oldest.last_seq, 1);
    // Only the oldest carries the unknown-loss flag.
    assert!(!manifest.generations[1].preceding_loss_unknown);
    assert!(!manifest.generations[1].origin_authenticated);
    // The fresh native generation is authenticated and continues the sequence.
    let active = manifest.active().unwrap();
    assert!(active.origin_authenticated);
    assert_eq!(active.first_seq, 3);
}

#[test]
fn native_generation_after_import_chains_from_the_import_seal() {
    let dir = TempDir::new().unwrap();
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");
    let root = dir.path().join("audit");
    let ledger = open_with_legacy(&root, &legacy).unwrap();
    ledger.append(entry("first-native")).unwrap();
    drop(ledger);

    // Reopening replays the whole structure, including across the import.
    let ledger = opened(&root);
    let manifest = ledger.manifest_snapshot();
    let imported = &manifest.generations[1];
    let active = manifest.active().unwrap();
    assert_eq!(active.chain_base, imported.final_tag.clone().unwrap());
    assert_eq!(ledger.status().global_last_seq, 3);
    ledger.verify_all().unwrap();
}

#[test]
fn legacy_import_is_one_shot_and_ignored_on_reopen() {
    let dir = TempDir::new().unwrap();
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");
    let root = dir.path().join("audit");
    drop(open_with_legacy(&root, &legacy).unwrap());

    // A second open with the same legacy directory must not re-import.
    let ledger = open_with_legacy(&root, &legacy).unwrap();
    assert_eq!(ledger.manifest_snapshot().generations.len(), 3);
    assert_eq!(ledger.status().imported_generations, 2);
    assert_eq!(ledger.status().recovery.imported_generations, 0);
}

#[test]
fn imported_ledger_refuses_v1_export_and_labels_unauthenticated_in_v2() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");
    let ledger = open_with_legacy(&dir.path().join("audit"), &legacy).unwrap();

    // A v1 document cannot say "unauthenticated origin", so refuse to emit one.
    assert_eq!(
        refusal_of(
            ledger
                .export(&out.path().join("v1"), ExportFormat::V1)
                .unwrap_err()
        ),
        RefuseReason::ExportV1IncompatibleMultiGeneration
    );
    let dest = out.path().join("v2");
    let receipt = ledger.export(&dest, ExportFormat::Auto).unwrap();
    assert!(receipt.schema.ends_with(".v2"));
    assert_eq!(receipt.unauthenticated_generations, 2);
    assert_eq!(
        verify_export(&dest, &keys())
            .unwrap()
            .unauthenticated_generations,
        2
    );
}

#[test]
fn v1_export_verifies_with_the_v2_verifier() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let dest = out.path().join("v1");
    let receipt = ledger.export(&dest, ExportFormat::V1).unwrap();
    drop(ledger);

    // Backward read: the current verifier still accepts a v1 export.
    let verified = verify_export(&dest, &keys()).unwrap();
    assert_eq!(verified.schema, receipt.schema);
    assert_eq!(verified.seal_id, receipt.seal_id);
    assert_eq!(verified.generations_verified, 1);
    assert_eq!(verified.holes, 0);
    assert!(verified.complete);
    // A v1 export keeps exactly the v1 file set at its root.
    let mut names: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["anchor.json", "export-manifest.json", "journal.jsonl"]
    );
}

#[test]
fn durable_dropped_entry_evidence_survives_restart() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    // The legacy ledger held this only in memory, so a restart erased the
    // evidence that evidence had been lost.
    ledger.record_dropped(7).unwrap();
    drop(ledger);

    let ledger = opened(dir.path());
    let gaps = ledger.status().recovery.durable_gaps;
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].lost_entries, 7);
    assert_eq!(gaps[0].after_seq, 5);
    assert!(
        gaps[0].journaled,
        "the loss is also chained into the journal"
    );
    let records: Vec<AuditRecord> = read_lines(&journal_of(dir.path(), "g-000001"))
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records[5].reason, Some(EntryReason::RecoveryDroppedEntries));
    assert_eq!(records[5].outcome, EntryOutcome::Uncertain);
}

#[test]
fn uncommitted_bootstrap_import_is_recovered_without_touching_legacy_bytes() {
    let dir = TempDir::new().unwrap();
    let current = "{\"tool\":\"auth\"}\n";
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}\n", current);
    let root = dir.path().join("audit");
    drop(open_with_legacy(&root, &legacy).unwrap());

    // Simulate a crash after the staged generations but before the manifest
    // commit: the marker is present, the manifest is not.
    let marker = super::import::BootstrapMarker::new(
        vec!["g-000001".into(), "g-000002".into(), "g-000003".into()],
        &keys(),
    )
    .unwrap();
    std::fs::write(
        super::import::bootstrap_path(&root),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    std::fs::remove_file(AuditLedger::manifest_path(&root)).unwrap();

    let ledger = open_with_legacy(&root, &legacy).unwrap();
    // The staged copies were cleared and the import re-ran cleanly.
    assert_eq!(ledger.manifest_snapshot().generations.len(), 3);
    assert!(!super::import::bootstrap_path(&root).exists());
    // The v1 sources were never touched, so nothing could be lost.
    assert_eq!(
        std::fs::read_to_string(legacy.join("audit.jsonl")).unwrap(),
        current
    );
}

#[test]
fn committed_bootstrap_marker_is_removed_without_replaying_import() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let manifest = ledger.manifest_snapshot();
    let ids = manifest
        .generations
        .iter()
        .map(|generation| generation.generation_id.clone())
        .collect();
    drop(ledger);

    let marker = super::import::BootstrapMarker::new(ids, &keys()).unwrap();
    std::fs::write(
        super::import::bootstrap_path(dir.path()),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
    let reopened = opened(dir.path());
    assert!(!super::import::bootstrap_path(dir.path()).exists());
    assert_eq!(reopened.status().global_last_seq, 5);
}

#[test]
fn migration_crash_cuts_before_each_phase_converge_from_legacy_inputs() {
    for staged_count in 0..=2 {
        let dir = TempDir::new().unwrap();
        let legacy = legacy_v1_dir(dir.path(), "{\"old\":1}\n", "{\"new\":2}\n");
        let root = dir.path().join("audit");
        std::fs::create_dir_all(root.join("generations")).unwrap();
        let ids = vec![
            "g-000001".to_string(),
            "g-000002".to_string(),
            "g-000003".to_string(),
        ];
        let marker = super::import::BootstrapMarker::new(ids.clone(), &keys()).unwrap();
        std::fs::write(
            super::import::bootstrap_path(&root),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        for id in ids.iter().take(staged_count) {
            std::fs::create_dir_all(root.join("generations").join(id)).unwrap();
        }

        let ledger = open_with_legacy(&root, &legacy).unwrap();
        assert_eq!(ledger.status().imported_generations, 2);
        assert!(!super::import::bootstrap_path(&root).exists());
        assert_eq!(
            std::fs::read(legacy.join("audit.jsonl")).unwrap(),
            b"{\"new\":2}\n"
        );
    }
}

#[test]
fn unterminated_legacy_line_is_counted_without_changing_legacy_bytes() {
    let dir = TempDir::new().unwrap();
    let legacy = legacy_v1_dir(dir.path(), "{\"a\":1}", "{\"b\":2}\n");
    let root = dir.path().join("audit");
    let ledger = open_with_legacy(&root, &legacy).unwrap();
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.generations[0].entry_count, 1);
    assert_eq!(manifest.generations[0].last_seq, 1);
    assert_eq!(
        std::fs::read(legacy.join("audit.jsonl.1")).unwrap(),
        b"{\"a\":1}"
    );
    assert!(ledger.verify_all().is_ok());
}

#[test]
fn intent_and_outcome_share_one_opaque_producer_identity() {
    let dir = TempDir::new().unwrap();
    let ledger = opened(dir.path());
    ledger
        .append(
            AuditEntryInput::new(
                "provider_attempt",
                EntryPhase::Intent,
                EntryOutcome::Accepted,
            )
            .with_intent_id("producer-intent-42")
            .with_request("request-secret"),
        )
        .unwrap();
    ledger
        .append(
            AuditEntryInput::new(
                "provider_attempt",
                EntryPhase::Outcome,
                EntryOutcome::Uncertain,
            )
            .with_intent_id("producer-intent-42")
            .with_request("request-secret"),
        )
        .unwrap();
    let path = journal_of(dir.path(), "g-000001");
    let body = std::fs::read_to_string(path).unwrap();
    assert!(body.contains("\"phase\":\"intent\""));
    assert!(body.contains("\"phase\":\"outcome\""));
    assert!(!body.contains("producer-intent-42"));
    assert!(!body.contains("request-secret"));
    drop(ledger);
    assert!(opened(dir.path()).verify_all().is_ok());
}

#[test]
fn symlinked_root_is_rejected() {
    #[cfg(unix)]
    {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(
            poison_of(open(&link).unwrap_err()),
            PoisonReason::SymlinkedPath
        );
    }
}

#[test]
fn audit_files_are_private() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let ledger = fresh(dir.path());
        ledger.rotate(RotationReason::Bytes).unwrap();
        drop(ledger);

        let mode = |path: PathBuf| {
            std::fs::metadata(&path)
                .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()))
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(AuditLedger::manifest_path(dir.path())), 0o600);
        assert_eq!(mode(journal_of(dir.path(), "g-000001")), 0o600);
        assert_eq!(
            mode(AuditLedger::anchor_path(dir.path(), "g-000001")),
            0o600
        );
        assert_eq!(
            mode(AuditLedger::generation_dir(dir.path(), "g-000002")),
            0o700
        );
    }
}

#[test]
fn a_second_writer_advancing_the_anchor_is_detected() {
    // Two handles on one root stand in for the second process that the
    // process-wide InstanceLock exists to prevent. The ledger must notice
    // rather than interleave two chains into one journal.
    let dir = TempDir::new().unwrap();
    let first = fresh(dir.path());
    let second = opened(dir.path());
    second.append(entry("from-the-other-writer")).unwrap();
    assert_eq!(
        poison_of(first.append(entry("from-the-first-writer")).unwrap_err()),
        PoisonReason::ConcurrentWriter
    );
    assert_eq!(
        first.status().poisoned,
        Some(PoisonReason::ConcurrentWriter)
    );
}

#[test]
fn installation_key_file_is_private_and_stable() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("keys").join("chain.key");
    let first = AuditKeys::load_or_create_file(&path).unwrap();
    let second = AuditKeys::load_or_create_file(&path).unwrap();
    assert_eq!(first.key_id(), second.key_id());
    assert_eq!(first.installation_id(), second.installation_id());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // A key another user could read fails closed rather than being repaired.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            poison_of(AuditKeys::load_or_create_file(&path).unwrap_err()),
            PoisonReason::KeyUnavailable
        );
    }
}

#[test]
fn a_failed_export_leaves_no_partial_destination() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();

    // Corrupt a sealed generation so the export's own verification refuses it.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_of(dir.path(), "g-000001"))
        .unwrap();
    file.write_all(b"{}\n").unwrap();
    file.sync_all().unwrap();

    let dest = out.path().join("partial");
    assert!(ledger.export(&dest, ExportFormat::Auto).is_err());
    assert!(
        !dest.exists(),
        "a refused export must not leave a directory that could pass for a sealed one"
    );
}

#[test]
fn concurrent_appends_never_issue_a_sequence_twice() {
    let dir = TempDir::new().unwrap();
    let ledger = Arc::new(opened(dir.path()));
    let mut handles = Vec::new();
    for worker in 0..4 {
        let ledger = Arc::clone(&ledger);
        handles.push(std::thread::spawn(move || {
            for index in 0..25 {
                ledger.append(entry(&format!("w{worker}.{index}"))).unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(ledger.status().global_last_seq, 100);
    drop(ledger);

    // Replaying the chain proves no sequence was issued twice: a duplicate
    // would break the strict +1 expectation during the scan.
    let ledger = opened(dir.path());
    assert_eq!(ledger.status().global_last_seq, 100);
    assert_eq!(ledger.verify_all().unwrap()[0].entry_count, 100);
}
