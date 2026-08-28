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

use super::authority::{AuditCapability, AuthorityGrant, AuthoritySource, LocalOperatorAuthority};
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

/// A ledger on a host where an operator is present for both capabilities.
///
/// The default provider grants nothing, so every test that deletes an
/// unexported generation or takes a raw export has to say so explicitly --
/// which is the point of the boundary.
fn open_with_operator(root: &Path) -> AuditResult<AuditLedger> {
    AuditLedger::open_with_options(
        root,
        keys(),
        AuditLedgerOptions {
            authority: Some(Arc::new(LocalOperatorAuthority::new([
                AuditCapability::PrivilegedRawExport,
                AuditCapability::RetainUnexported,
            ]))),
            ..Default::default()
        },
    )
}

fn operator_ledger(root: &Path) -> AuditLedger {
    open_with_operator(root).expect("ledger opens")
}

/// Five committed entries in generation 1, on an operator host.
fn fresh_with_operator(root: &Path) -> AuditLedger {
    let ledger = operator_ledger(root);
    for index in 0..5 {
        ledger
            .append(entry(&format!("op.{index}")))
            .expect("append");
    }
    ledger
}

fn retain_grant(ledger: &AuditLedger, generation_id: &str) -> AuthorityGrant {
    ledger
        .issue_authority(AuditCapability::RetainUnexported, generation_id)
        .expect("operator grant")
}

fn raw_export_grant(ledger: &AuditLedger) -> AuthorityGrant {
    ledger
        .issue_authority(
            AuditCapability::PrivilegedRawExport,
            super::authority::PRIVILEGED_RAW_EXPORT_SUBJECT,
        )
        .expect("operator grant")
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
    // A different installation key is reported as a key mismatch rather than
    // as tampering; both fail closed identically.
    assert_eq!(poison_of(error), PoisonReason::KeyMismatch);
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
        .retain(RetentionRequest::exported_under("g-000001", "seal-x"))
        .unwrap_err();
    assert_eq!(refusal_of(error), RefuseReason::GenerationIsActive);
}

#[test]
fn retention_refuses_a_seal_this_ledger_never_issued() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    // A caller-supplied seal id is a lookup key, never a claim. Before the
    // registry existed, any non-empty string was accepted as proof of export.
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::exported_under(
                    "g-000001",
                    "seal-that-never-existed"
                ))
                .unwrap_err()
        ),
        RefuseReason::ExportSealUnknown
    );
    assert!(ledger.manifest_snapshot().tombstones.is_empty());
}

#[test]
fn deleting_an_unexported_generation_needs_an_operator_host() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    // The default provider grants nothing, so the last copy of a range cannot
    // be destroyed at all on a host that never said an operator was present.
    assert_eq!(
        refusal_of(
            ledger
                .issue_authority(AuditCapability::RetainUnexported, "g-000001")
                .unwrap_err()
        ),
        RefuseReason::AuthorityUnavailable
    );
    assert!(ledger.manifest_snapshot().tombstones.is_empty());
}

#[test]
fn an_unexported_deletion_records_the_grant_that_allowed_it() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let grant = retain_grant(&ledger, "g-000001");
    let grant_id = grant.grant_id().to_string();
    let receipt = ledger
        .retain(RetentionRequest::under_grant("g-000001", grant))
        .unwrap();

    let tombstone = ledger.manifest_snapshot().tombstones[0].clone();
    assert!(tombstone.allow_unexported);
    assert_eq!(
        tombstone.authority_grant_id.as_deref(),
        Some(grant_id.as_str())
    );
    // Permanently honest about what stood behind it: an operator act on this
    // host, not a verified principal (#460/#461).
    assert_eq!(
        tombstone.authority_source,
        Some(AuthoritySource::LocalOperator)
    );
    assert_eq!(
        receipt.authority_grant_id.as_deref(),
        Some(grant_id.as_str())
    );
    assert!(receipt.export_seal_id.is_none());
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
        .retain(RetentionRequest::exported_under(
            "g-000001",
            &receipt.seal_id,
        ))
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
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    let ledger = ledger.with_crash_at(CrashPoint::T3Committed);
    assert!(ledger
        .retain(RetentionRequest::under_grant(
            "g-000001",
            retain_grant(&ledger, "g-000001")
        ))
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
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    let ledger = ledger.with_crash_at(CrashPoint::T4Removed);
    assert!(ledger
        .retain(RetentionRequest::under_grant(
            "g-000001",
            retain_grant(&ledger, "g-000001")
        ))
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
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_of(dir.path(), "g-000001"))
        .unwrap();
    file.write_all(b"{}\n").unwrap();
    file.sync_all().unwrap();

    let error = ledger
        .retain(RetentionRequest::under_grant(
            "g-000001",
            retain_grant(&ledger, "g-000001"),
        ))
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
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Bytes).unwrap();
    ledger.append(entry("post")).unwrap();
    ledger
        .retain(RetentionRequest::under_grant(
            "g-000001",
            retain_grant(&ledger, "g-000001"),
        ))
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

    let receipt = ledger
        .export(&out.path().join("out"), ExportFormat::Auto)
        .unwrap();

    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "history was rewritten"
    );
    assert_eq!(ledger.manifest_snapshot().generations.len(), 1);
    // The manifest *does* advance, by exactly one additive fact: the seal this
    // export issued and re-verified. Retention consults that registry instead
    // of trusting a caller-supplied seal id, so recording it is what makes
    // "this range was exported" checkable at all. No generation, journal byte,
    // tombstone or chain tag changes.
    let after = ledger.manifest_snapshot();
    assert_eq!(after.manifest_epoch, epoch + 1);
    assert_eq!(after.seals.len(), 1);
    assert_eq!(after.seals[0].seal_id, receipt.seal_id);
    assert!(after.tombstones.is_empty());
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

/// The same, on a host where an operator is present for both capabilities.
fn open_with_legacy_operator(root: &Path, legacy: &Path) -> AuditResult<AuditLedger> {
    AuditLedger::open_with_options(
        root,
        keys(),
        AuditLedgerOptions {
            legacy_v1_dir: Some(legacy.to_path_buf()),
            authority: Some(Arc::new(LocalOperatorAuthority::new([
                AuditCapability::PrivilegedRawExport,
                AuditCapability::RetainUnexported,
            ]))),
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
    let ledger = open_with_legacy_operator(&dir.path().join("audit"), &legacy).unwrap();

    // A v1 document cannot say "unauthenticated origin", so refuse to emit one.
    assert_eq!(
        refusal_of(
            ledger
                .export(&out.path().join("v1"), ExportFormat::V1)
                .unwrap_err()
        ),
        RefuseReason::ExportV1IncompatibleMultiGeneration
    );
    // A public export withholds the imported generations rather than carrying
    // them: preserved verbatim means never redacted to the v2 rules.
    let dest = out.path().join("v2");
    let receipt = ledger.export(&dest, ExportFormat::Auto).unwrap();
    assert!(receipt.schema.ends_with(".v2"));
    assert_eq!(receipt.unauthenticated_generations, 0);
    assert_eq!(receipt.withheld, 2);
    assert!(!receipt.complete);
    let verified = verify_export(&dest, &keys()).unwrap();
    assert_eq!(verified.unauthenticated_generations, 0);
    assert_eq!(verified.withheld, 2);

    // Privileged raw preservation is the only scope that carries them, and it
    // declares that it does.
    let raw = ledger
        .export_privileged_raw(
            &out.path().join("raw"),
            ExportFormat::Auto,
            &raw_export_grant(&ledger),
        )
        .unwrap();
    assert_eq!(raw.unauthenticated_generations, 2);
    assert!(raw.contains_unauthenticated_legacy);
    assert!(raw.complete);
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

// ------------------------------------------------- structural barrier (#462)

#[test]
fn a_structural_transaction_holds_the_barrier_against_appends() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = TempDir::new().unwrap();
    let held = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&held);
    let ledger = opened(dir.path()).with_structural_observer(Arc::new(move |ledger| {
        // Deterministic: this asserts the lock is held, not that a race lost.
        flag.store(!ledger.inner_is_unlocked(), Ordering::SeqCst);
    }));
    ledger.append(entry("before")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    assert!(
        held.load(Ordering::SeqCst),
        "a rotation must hold the inner lock for its whole transaction"
    );
}

#[test]
fn a_manifest_commit_is_refused_when_another_writer_moved_the_epoch() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let manifest = ledger.manifest_snapshot();

    // Stand in for a second process: advance the committed epoch underneath us.
    let mut behind_our_back = manifest.clone();
    behind_our_back.manifest_epoch += 5;
    behind_our_back.seal(&keys()).unwrap();
    std::fs::write(
        AuditLedger::manifest_path(dir.path()),
        serde_json::to_vec(&behind_our_back).unwrap(),
    )
    .unwrap();

    // The swap fails rather than silently overwriting the other writer.
    assert_eq!(
        poison_of(ledger.commit_manifest(manifest).unwrap_err()),
        PoisonReason::ConcurrentWriter
    );
}

#[test]
fn concurrent_appends_during_a_rotation_never_strand_an_entry() {
    let dir = TempDir::new().unwrap();
    let ledger = Arc::new(opened(dir.path()));
    for index in 0..5 {
        ledger.append(entry(&format!("pre.{index}"))).unwrap();
    }

    // Appenders run against the ledger while a rotation is in flight. The
    // assertions are invariants, not timings: whichever order the barrier
    // admits them in, the chain must replay and no sequence may repeat.
    let mut handles = Vec::new();
    for worker in 0..4 {
        let ledger = Arc::clone(&ledger);
        handles.push(std::thread::spawn(move || {
            for index in 0..20 {
                ledger.append(entry(&format!("w{worker}.{index}"))).unwrap();
            }
        }));
    }
    let rotator = Arc::clone(&ledger);
    let rotation = std::thread::spawn(move || rotator.rotate(RotationReason::Operator));
    for handle in handles {
        handle.join().unwrap();
    }
    rotation.join().unwrap().expect("rotation");

    // 5 pre + 80 concurrent + sealing + opened.
    let total = 5 + 80 + 2;
    assert_eq!(ledger.status().global_last_seq, total);
    let verified = ledger.verify_all().unwrap();
    assert_eq!(verified.len(), 2);
    // Every issued sequence lands in exactly one generation, contiguously.
    let manifest = ledger.manifest_snapshot();
    let mut expected = 1u64;
    for generation in &manifest.generations {
        assert_eq!(generation.first_seq, expected);
        expected = generation.last_seq + 1;
    }
    assert_eq!(
        verified.iter().map(|v| v.entry_count).sum::<u64>(),
        total,
        "an entry was stranded outside a sealed range"
    );
}

#[test]
fn a_rotation_racing_a_retention_never_drops_a_generation_or_regresses_the_epoch() {
    let dir = TempDir::new().unwrap();
    let ledger = Arc::new(operator_ledger(dir.path()));
    ledger.append(entry("first")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("second")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("third")).unwrap();
    // Taken before the race: issuing a grant appends, and the point of the
    // test is the rotation/retention overlap, not the issuing.
    let grant = retain_grant(&ledger, "g-000001");
    let epoch_before = ledger.manifest_snapshot().manifest_epoch;

    let retainer = Arc::clone(&ledger);
    let retention = std::thread::spawn(move || {
        retainer.retain(RetentionRequest::under_grant("g-000001", grant))
    });
    let rotator = Arc::clone(&ledger);
    let rotation = std::thread::spawn(move || rotator.rotate(RotationReason::Operator));
    retention.join().unwrap().expect("retention");
    rotation.join().unwrap().expect("rotation");

    let manifest = ledger.manifest_snapshot();
    assert!(manifest.manifest_epoch > epoch_before, "epoch regressed");
    assert_eq!(manifest.retention_epoch, 1);
    assert_eq!(manifest.tombstones.len(), 1);
    // Four generations: three sealed or tombstoned, one active. None dropped.
    assert_eq!(manifest.generations.len(), 4);
    let mut expected = 1u64;
    for generation in &manifest.generations {
        assert_eq!(generation.first_seq, expected, "a generation was dropped");
        expected = generation.last_seq + 1;
    }
    ledger.verify_all().unwrap();
}

#[test]
fn an_arbitrary_op_string_cannot_carry_a_secret_fragment() {
    let dir = TempDir::new().unwrap();
    let ledger = opened(dir.path());
    let generation = ledger.status().active_generation_id;
    ledger
        .append(AuditEntryInput::new(
            "/private/workspace sk-live-SECRET",
            EntryPhase::Outcome,
            EntryOutcome::Accepted,
        ))
        .unwrap();
    let body = read_lines(&journal_of(dir.path(), &generation)).join("\n");
    assert!(body.contains("\"op\":\"invalid_op\""));
    assert!(!body.contains("sk-live-SECRET"));
    assert!(!body.contains("/private/workspace"));
    // A normal tool name survives untouched.
    assert_eq!(
        super::documents::sanitize_op("ptah_submit_task"),
        "ptah_submit_task"
    );
    assert_eq!(
        super::documents::sanitize_op("audit.generation.opened"),
        "audit.generation.opened"
    );
}

#[test]
fn an_append_that_cannot_land_poisons_rather_than_reporting_clean() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    let journal = journal_of(dir.path(), &generation);

    // Replace the journal with a directory. Unlike a mode change this is not
    // bypassed by a privileged uid, so the test means the same thing whoever
    // runs it.
    std::fs::remove_file(&journal).unwrap();
    std::fs::create_dir(&journal).unwrap();

    assert!(ledger.append(entry("cannot-land")).is_err());
    assert_eq!(
        ledger.status().poisoned,
        Some(PoisonReason::PartialPersistence),
        "a failed append must not leave the ledger reporting clean"
    );
    // And a poisoned ledger refuses every later structural operation.
    assert!(ledger.rotate(RotationReason::Operator).is_err());
    assert!(ledger.append(entry("still-refused")).is_err());
}

#[test]
fn a_failed_append_still_leaves_durable_evidence_on_disk() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    let generation = ledger.status().active_generation_id;
    let journal = journal_of(dir.path(), &generation);

    // Make the journal unwritable in a way a privileged uid cannot bypass.
    std::fs::remove_file(&journal).unwrap();
    std::fs::create_dir(&journal).unwrap();
    assert!(ledger.append(entry("cannot-land")).is_err());

    // The gap file is written before the chained record is attempted, so the
    // loss survives even when the journal itself cannot be written. This is
    // what stops a shutdown whose own record failed from looking clean.
    assert!(
        ledger.record_dropped(2).is_err(),
        "the chained record cannot land"
    );
    let gap_path = dir.path().join("gap.json");
    assert!(
        gap_path.is_file(),
        "durable gap evidence must exist on disk"
    );
    let recorded: super::documents::GapFile =
        serde_json::from_slice(&std::fs::read(&gap_path).unwrap()).unwrap();
    recorded
        .verify(&keys())
        .expect("the gap file is authenticated");
    assert_eq!(recorded.gaps.iter().map(|g| g.lost_entries).sum::<u64>(), 2);
    assert!(
        recorded.gaps.iter().all(|g| !g.journaled),
        "an unjournaled loss must stay marked unjournaled"
    );
}

// ------------------------------------------- accepted-but-not-journaled (P0)

#[test]
fn entries_accepted_by_a_queue_but_never_journaled_are_reported_as_bounded_uncertainty() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        // An async producer queue accepted three entries. Before this marker
        // existed, `enqueue_audit` returned "accepted" for entries that lived
        // only in memory, and a crash here left no trace of them at all.
        for _ in 0..3 {
            ledger.note_accepted(256).unwrap();
        }
        assert!(dir.path().join("pending.json").is_file());
        // The process dies: nothing settles, nothing is appended.
    }

    let ledger = opened(dir.path());
    let gaps = ledger.status().recovery.durable_gaps;
    let pending: Vec<_> = gaps
        .iter()
        .filter(|gap| gap.reason == EntryReason::AcceptedNotJournaled)
        .collect();
    assert_eq!(pending.len(), 1, "one marker, one bounded gap");
    // Nothing is *known* lost, and up to a full queue may be. Reporting a bare
    // zero would read as "nothing was lost"; reporting 256 would invent losses.
    assert_eq!(pending[0].lost_entries, 0);
    assert_eq!(pending[0].max_lost_entries, Some(256));
    assert!(
        !dir.path().join("pending.json").exists(),
        "the marker is consumed once it has become durable evidence"
    );

    // The uncertainty is chained into the journal, not only in the gap file.
    let journal = std::fs::read_to_string(journal_of(dir.path(), "g-000001")).unwrap();
    assert!(
        journal.contains("accepted_not_journaled"),
        "the loss must be journaled under its own reason"
    );
    ledger.verify_all().unwrap();
}

#[test]
fn a_settled_queue_leaves_no_marker_and_no_uncertainty() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.note_accepted(256).unwrap();
        ledger.note_accepted(256).unwrap();
        assert_eq!(ledger.in_flight(), 2);
        ledger.append(entry("queued.one")).unwrap();
        ledger.note_settled().unwrap();
        assert!(
            dir.path().join("pending.json").is_file(),
            "one entry is still in flight"
        );
        ledger.append(entry("queued.two")).unwrap();
        ledger.note_settled().unwrap();
        assert_eq!(ledger.in_flight(), 0);
        assert!(!dir.path().join("pending.json").exists());
    }
    let ledger = opened(dir.path());
    assert!(ledger
        .status()
        .recovery
        .durable_gaps
        .iter()
        .all(|gap| gap.reason != EntryReason::AcceptedNotJournaled));
}

#[test]
fn a_consumed_pending_marker_is_not_reported_again_on_the_next_open() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.note_accepted(64).unwrap();
    }
    let first = opened(dir.path());
    let reported = |ledger: &AuditLedger| {
        ledger
            .status()
            .recovery
            .durable_gaps
            .iter()
            .filter(|gap| gap.reason == EntryReason::AcceptedNotJournaled)
            .count()
    };
    assert_eq!(reported(&first), 1);
    drop(first);

    // The gap is permanent evidence, but the *marker* is gone, so a restart
    // does not manufacture a second episode of uncertainty.
    let second = opened(dir.path());
    assert_eq!(reported(&second), 1);
    let journal = std::fs::read_to_string(journal_of(dir.path(), "g-000001")).unwrap();
    assert_eq!(
        journal.matches("accepted_not_journaled").count(),
        1,
        "the same loss must not be re-journaled on every open"
    );
}

// ------------------------------------------------ export seal authority (P0)

#[test]
fn retention_refuses_a_seal_whose_export_withheld_the_range() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let legacy = TempDir::new().unwrap();
    std::fs::write(
        legacy.path().join("audit.jsonl"),
        "{\"tool\":\"legacy\",\"detail\":\"/home/someone/secret/path\"}\n",
    )
    .unwrap();
    let ledger = AuditLedger::open_with_options(
        dir.path(),
        keys(),
        AuditLedgerOptions {
            legacy_v1_dir: Some(legacy.path().to_path_buf()),
            ..Default::default()
        },
    )
    .expect("ledger with imported legacy bytes");
    ledger.append(entry("post.import")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("after")).unwrap();

    // A public export withholds the imported generation: it copies none of its
    // bytes. Treating that seal as proof the range was preserved would let the
    // only copy be deleted on the strength of an export that never had it.
    let receipt = ledger
        .export(&out.path().join("public"), ExportFormat::Auto)
        .unwrap();
    assert!(receipt.withheld >= 1);
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::exported_under(
                    "g-000001",
                    &receipt.seal_id
                ))
                .unwrap_err()
        ),
        RefuseReason::ExportSealDoesNotCover
    );
    assert!(ledger.manifest_snapshot().tombstones.is_empty());
}

#[test]
fn retention_accepts_only_the_exact_range_a_seal_carried() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("second.generation")).unwrap();

    // Taken while g-000002 is still active, so the export carries a *prefix*
    // of it: two entries, not the four it will hold by the time retention runs.
    let seal = ledger
        .export(&out.path().join("prefix"), ExportFormat::Auto)
        .unwrap();
    ledger.append(entry("after.the.export")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("third.generation")).unwrap();

    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::exported_under("g-000002", &seal.seal_id))
                .unwrap_err()
        ),
        RefuseReason::ExportSealDoesNotCover,
        "an export of a shorter prefix is not a copy of what would be deleted"
    );
    // g-000001 was already sealed when the export ran, so its bytes are
    // byte-identical to what the export carried.
    ledger
        .retain(RetentionRequest::exported_under("g-000001", &seal.seal_id))
        .expect("the exact range this seal carried");
}

// ------------------------------------------------- capability authority (P0)

#[test]
fn a_raw_export_needs_an_operator_host() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    // The default provider grants nothing, so unredacted bytes cannot leave.
    assert_eq!(
        refusal_of(
            ledger
                .issue_authority(
                    AuditCapability::PrivilegedRawExport,
                    super::authority::PRIVILEGED_RAW_EXPORT_SUBJECT
                )
                .unwrap_err()
        ),
        RefuseReason::AuthorityUnavailable
    );
    // The refusal is itself journaled: a denied attempt to take unredacted
    // history out of the ledger is exactly as interesting as a granted one.
    let journal = std::fs::read_to_string(journal_of(dir.path(), "g-000001")).unwrap();
    assert!(journal.contains("privileged_raw_export"));
}

#[test]
fn an_operator_host_grants_only_the_capabilities_it_named() {
    let dir = TempDir::new().unwrap();
    let ledger = AuditLedger::open_with_options(
        dir.path(),
        keys(),
        AuditLedgerOptions {
            authority: Some(Arc::new(LocalOperatorAuthority::new([
                AuditCapability::PrivilegedRawExport,
            ]))),
            ..Default::default()
        },
    )
    .unwrap();
    ledger.append(entry("first")).unwrap();
    ledger
        .issue_authority(
            AuditCapability::PrivilegedRawExport,
            super::authority::PRIVILEGED_RAW_EXPORT_SUBJECT,
        )
        .expect("the named capability");
    assert_eq!(
        refusal_of(
            ledger
                .issue_authority(AuditCapability::RetainUnexported, "g-000001")
                .unwrap_err()
        ),
        RefuseReason::AuthorityUnavailable,
        "a host that needs raw exports must not thereby gain deletion"
    );
}

#[test]
fn a_forged_grant_is_refused() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("after")).unwrap();

    let grant = retain_grant(&ledger, "g-000001");
    // Re-issued from the same fields under a key the ledger does not have.
    let forged: AuthorityGrant =
        serde_json::from_value(json_with_mac(&grant, "0".repeat(64))).unwrap();
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::under_grant("g-000001", forged))
                .unwrap_err()
        ),
        RefuseReason::AuthorityInvalid
    );
    // The real one still works, so the refusal was about the tag, not the shape.
    ledger
        .retain(RetentionRequest::under_grant("g-000001", grant))
        .expect("a genuine grant");
    assert_eq!(ledger.manifest_snapshot().tombstones.len(), 1);
}

#[test]
fn a_grant_is_single_use() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());

    // Privileged raw export is where a replay is actually reachable: retention
    // tombstones its subject, so a second use there is caught one check
    // earlier. This exercises the same consumed-grant check directly.
    let grant = raw_export_grant(&ledger);
    let replay = grant.clone();
    ledger
        .export_privileged_raw(&out.path().join("first"), ExportFormat::Auto, &grant)
        .expect("first use");
    assert_eq!(
        refusal_of(
            ledger
                .export_privileged_raw(&out.path().join("second"), ExportFormat::Auto, &replay)
                .unwrap_err()
        ),
        RefuseReason::AuthorityAlreadyConsumed,
        "a captured grant must authorize nothing a second time, even in its TTL"
    );
    assert!(
        !out.path().join("second").exists(),
        "a refused export leaves no destination behind"
    );
}

#[test]
fn a_grant_is_bound_to_the_generation_it_names() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("second")).unwrap();
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("third")).unwrap();

    let grant = retain_grant(&ledger, "g-000001");
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::under_grant("g-000002", grant))
                .unwrap_err()
        ),
        RefuseReason::AuthorityScopeMismatch
    );
    assert!(ledger.manifest_snapshot().tombstones.is_empty());
}

#[test]
fn a_capability_grant_cannot_be_spent_on_another_capability() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("second")).unwrap();

    // Minted for a deletion, presented for an export of unredacted bytes.
    let deletion = retain_grant(&ledger, "g-000001");
    assert_eq!(
        refusal_of(
            ledger
                .export_privileged_raw(&out.path().join("raw"), ExportFormat::Auto, &deletion)
                .unwrap_err()
        ),
        RefuseReason::AuthorityScopeMismatch
    );
    // Unspent, so the capability it *was* minted for still works.
    ledger
        .retain(RetentionRequest::under_grant("g-000001", deletion))
        .expect("the capability it was minted for");
}

#[test]
fn a_spent_grant_id_is_recorded_in_the_manifest() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("second")).unwrap();

    let grant = retain_grant(&ledger, "g-000001");
    let replay = grant.clone();
    ledger
        .retain(RetentionRequest::under_grant("g-000001", grant))
        .expect("first use");
    let manifest = ledger.manifest_snapshot();
    assert_eq!(manifest.consumed_grants.len(), 1);
    assert_eq!(
        manifest.consumed_grants[0].grant_id,
        replay.grant_id(),
        "the spent id is recorded in the same manifest write as the tombstone"
    );
    assert_eq!(
        manifest.consumed_grants[0].source,
        AuthoritySource::LocalOperator
    );
}

#[test]
fn a_grant_from_another_installation_is_refused() {
    let dir = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let ledger = fresh_with_operator(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("after")).unwrap();

    let foreign = AuditLedger::open_with_options(
        other.path(),
        foreign_keys(),
        AuditLedgerOptions {
            authority: Some(Arc::new(LocalOperatorAuthority::new([
                AuditCapability::RetainUnexported,
            ]))),
            ..Default::default()
        },
    )
    .unwrap();
    foreign.append(entry("first")).unwrap();
    let stolen = foreign
        .issue_authority(AuditCapability::RetainUnexported, "g-000001")
        .unwrap();
    assert_eq!(
        refusal_of(
            ledger
                .retain(RetentionRequest::under_grant("g-000001", stolen))
                .unwrap_err()
        ),
        RefuseReason::AuthorityInvalid,
        "a grant minted elsewhere authorizes nothing here"
    );
}

// ------------------------------------------------------- concurrency (P0/P1)

#[test]
fn concurrent_drop_recorders_never_erase_each_others_evidence() {
    let dir = TempDir::new().unwrap();
    let ledger = Arc::new(fresh(dir.path()));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let recorder = Arc::clone(&ledger);
        handles.push(std::thread::spawn(move || {
            for _ in 0..4 {
                recorder.record_dropped(1).expect("record a drop");
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }

    // Read-modify-write of the gap file runs under one lock. Releasing it
    // before the atomic write let an older snapshot land after a newer one and
    // silently erase the losses in between.
    let recorded: super::documents::GapFile =
        serde_json::from_slice(&std::fs::read(dir.path().join("gap.json")).unwrap()).unwrap();
    recorded.verify(&keys()).expect("authenticated");
    assert_eq!(
        recorded
            .gaps
            .iter()
            .map(|gap| gap.lost_entries)
            .sum::<u64>(),
        32,
        "every recorded loss must survive concurrent recorders"
    );
    ledger.verify_all().unwrap();
}

#[test]
fn export_completeness_is_decided_inside_the_structural_barrier() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&observed);
    let ledger = AuditLedger::open(dir.path(), keys())
        .unwrap()
        .with_structural_observer(Arc::new(move |ledger: &AuditLedger| {
            // Checking poisoned/open-intent state before taking the barrier let
            // an intent open in the check-to-lock window, after which the
            // export would claim a completeness that was no longer true.
            assert!(
                !ledger.inner_is_unlocked(),
                "the export decided completeness without holding the barrier"
            );
            flag.store(true, Ordering::SeqCst);
        }));
    ledger.append(entry("first")).unwrap();
    let receipt = ledger
        .export(&out.path().join("sealed"), ExportFormat::Auto)
        .unwrap();
    assert!(receipt.complete);
    assert!(
        observed.load(Ordering::SeqCst),
        "the barrier was never taken"
    );
}

#[test]
fn a_concurrent_intent_never_yields_a_falsely_complete_export() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = Arc::new(fresh(dir.path()));
    let writer = Arc::clone(&ledger);
    let stop = Arc::new(AtomicBool::new(false));
    let halt = Arc::clone(&stop);
    let churn = std::thread::spawn(move || {
        let mut round = 0u32;
        while !halt.load(Ordering::SeqCst) {
            let producer = format!("intent-{round}");
            writer
                .append(
                    AuditEntryInput::new("race.op", EntryPhase::Intent, EntryOutcome::Accepted)
                        .with_producer(&producer),
                )
                .expect("intent");
            writer
                .append(
                    AuditEntryInput::new("race.op", EntryPhase::Outcome, EntryOutcome::Accepted)
                        .with_producer(&producer),
                )
                .expect("outcome");
            round = round.wrapping_add(1);
        }
    });

    let mut complete = 0usize;
    for round in 0..60 {
        let dest = out.path().join(format!("export-{round}"));
        match ledger.export(&dest, ExportFormat::Auto) {
            Ok(receipt) => {
                // A complete export must tile every sequence the ledger had
                // committed when the barrier was taken -- no hole, no short
                // range, and it must still verify standalone.
                let verified = verify_export(&dest, &keys()).expect("export verifies");
                assert_eq!(verified.complete, receipt.complete);
                if receipt.complete {
                    assert_eq!(receipt.global_first_seq, 1);
                    complete += 1;
                }
            }
            // The only legal refusal here: an intent was open at the barrier.
            Err(error) => assert_eq!(
                refusal_of(error),
                RefuseReason::OpenIntentsPresent,
                "an export must refuse rather than misreport"
            ),
        }
    }
    stop.store(true, Ordering::SeqCst);
    churn.join().unwrap();
    assert!(complete > 0, "no export ever completed");
    ledger.verify_all().unwrap();
}

/// Re-serialize a grant with a substituted tag, to forge one without the key.
fn json_with_mac(grant: &AuthorityGrant, mac: String) -> serde_json::Value {
    let mut value = serde_json::to_value(grant).expect("serialize grant");
    value["mac"] = serde_json::Value::String(mac);
    value
}

// -------------------------------- sealed-history verification at reopen (P0)

#[test]
fn tampering_with_a_sealed_generation_is_caught_at_the_next_open() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.rotate(RotationReason::Operator).unwrap();
        ledger.append(entry("after.rotate")).unwrap();
    }
    // g-000001 is sealed. Rewrite one of its committed lines: before this
    // check, only the *active* journal was verified at open, so a tampered
    // sealed generation reopened with a healthy status and stayed healthy
    // until someone happened to export, retain, or call verify_all.
    let path = journal_of(dir.path(), "g-000001");
    let mut lines = read_lines(&path);
    lines[2] = lines[2].replace("op.2", "op.X");
    write_lines(&path, &lines);

    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::SealedGenerationChanged
    );
}

#[test]
fn truncating_a_sealed_generation_is_caught_at_the_next_open() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.rotate(RotationReason::Operator).unwrap();
        ledger.append(entry("after.rotate")).unwrap();
    }
    let path = journal_of(dir.path(), "g-000001");
    let mut lines = read_lines(&path);
    lines.truncate(lines.len() - 1);
    write_lines(&path, &lines);

    // Dropping whole entries changes both the length and the digest, and the
    // digest lives inside the manifest MAC.
    assert_eq!(
        poison_of(open(dir.path()).unwrap_err()),
        PoisonReason::SealedGenerationChanged
    );
}

#[test]
fn an_untouched_sealed_generation_still_opens_cleanly() {
    let dir = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.rotate(RotationReason::Operator).unwrap();
        ledger.append(entry("after.rotate")).unwrap();
        ledger.rotate(RotationReason::Operator).unwrap();
        ledger.append(entry("third")).unwrap();
    }
    // The check must not be a boot-time false positive generator.
    let ledger = opened(dir.path());
    assert!(ledger.status().poisoned.is_none());
    ledger.verify_all().unwrap();
}

#[test]
fn a_tombstoned_generation_is_not_expected_to_be_on_disk_at_open() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    {
        let ledger = fresh(dir.path());
        ledger.rotate(RotationReason::Operator).unwrap();
        ledger.append(entry("after")).unwrap();
        let receipt = ledger
            .export(&out.path().join("sealed"), ExportFormat::Auto)
            .unwrap();
        ledger
            .retain(RetentionRequest::exported_under(
                "g-000001",
                &receipt.seal_id,
            ))
            .unwrap();
    }
    // The sealed-generation sweep must skip tombstoned ranges: their bytes are
    // gone by authorized deletion, which is not tampering.
    let ledger = opened(dir.path());
    assert!(ledger.status().poisoned.is_none());
    assert_eq!(ledger.status().tombstones, 1);
}

// ------------------------------------------------- counter exhaustion (P1)

#[test]
fn a_sequence_at_its_maximum_fails_closed_instead_of_reusing_a_position() {
    let dir = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    // Drive the live tail to u64::MAX. Saturating arithmetic would have
    // reissued MAX forever, giving two different entries one authenticated
    // position in the chain.
    ledger.force_last_seq_for_test(u64::MAX).unwrap();
    assert_eq!(
        poison_of(ledger.append(entry("overflow")).unwrap_err()),
        PoisonReason::SequenceExhausted
    );
}

// ------------------------------------- export manifest authentication (P1)

#[test]
fn a_substituted_manifest_inside_an_export_is_refused() {
    let dir = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let ledger = fresh(dir.path());
    ledger.rotate(RotationReason::Operator).unwrap();
    ledger.append(entry("after")).unwrap();
    let dest = out.path().join("sealed");
    ledger.export(&dest, ExportFormat::Auto).unwrap();
    verify_export(&dest, &keys()).expect("the untouched export verifies");

    // A manifest authentic under a *different* installation key.
    let foreign =
        AuditLedger::open_with_options(other.path(), foreign_keys(), AuditLedgerOptions::default())
            .unwrap();
    foreign.append(entry("elsewhere")).unwrap();
    std::fs::copy(
        AuditLedger::manifest_path(other.path()),
        dest.join("manifest.json"),
    )
    .unwrap();

    // The copied ledger manifest was previously carried unauthenticated, so a
    // swapped one could misdescribe generations, tombstones and retention to
    // anyone who trusted the export directory as a whole.
    assert!(
        verify_export(&dest, &keys()).is_err(),
        "a substituted manifest must not verify"
    );
}
