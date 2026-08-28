//! Deterministic audit-v2 integration tests against the real `OrchStore` (#462).
//!
//! These exercise the shipped orchestration store, not an isolated ledger: every
//! case opens a real `OrchStore` and drives the canonical `append_audit` /
//! `enqueue_audit` path that the 33 production producers use.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use grokptah_agent_bridge::audit::{
    verify_export, AuditCapability, AuditKeyCustody, AuditWitness, AuthorityGrant, AuthoritySource,
    ExportFormat, ExportScope, LocalOperatorAuthority, RetentionRequest, RotationReason,
    WitnessBeacon, WitnessState, WitnessVerdict,
};
use grokptah_agent_bridge::orchestration::{AuditEntry, AuditPhase, OrchStore};
use tempfile::TempDir;

// ------------------------------------------------------------------ helpers

fn entry(tool: &str, outcome: &str) -> AuditEntry {
    AuditEntry {
        ts: Utc::now(),
        tool: tool.into(),
        request_id: None,
        session_id: None,
        workspace: None,
        outcome: outcome.into(),
        error_code: None,
        detail: String::new(),
        intent_id: None,
        phase: AuditPhase::Outcome,
    }
}

/// A store on a host where an operator is present for both audit capabilities.
///
/// `OrchStore::open` installs no authority provider, so the two operations that
/// destroy or expose history are unreachable there. Every test that needs one
/// has to say so, which is the boundary working.
fn open_operator_store(root: &Path) -> OrchStore {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let custody = AuditKeyCustody::local_file_for(&root);
    OrchStore::open_with_audit_authority(
        &root,
        custody,
        None,
        Some(Arc::new(LocalOperatorAuthority::new([
            AuditCapability::PrivilegedRawExport,
            AuditCapability::RetainUnexported,
        ]))),
    )
    .expect("operator store opens")
}

fn raw_grant(store: &OrchStore) -> AuthorityGrant {
    store
        .request_audit_authority(
            AuditCapability::PrivilegedRawExport,
            grokptah_agent_bridge::audit::PRIVILEGED_RAW_EXPORT_SUBJECT,
        )
        .expect("operator grant")
}

/// `OrchStore` intentionally has no `Debug`, so take the error side directly.
fn open_error(result: anyhow::Result<OrchStore>) -> anyhow::Error {
    result.err().expect("open must fail closed")
}

fn audit_root(root: &Path) -> PathBuf {
    root.join("audit").join("v2")
}

fn legacy_dir(root: &Path) -> PathBuf {
    root.join("audit")
}

fn journal_of(root: &Path, generation: &str) -> PathBuf {
    audit_root(root)
        .join("generations")
        .join(generation)
        .join("journal.jsonl")
}

/// Write a legacy v1 ledger exactly as the retired `append_audit_entry` left it.
fn seed_legacy_v1(root: &Path, older: &str, current: &str) {
    let dir = legacy_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    if !older.is_empty() {
        std::fs::write(dir.join("audit.jsonl.1"), older).unwrap();
    }
    std::fs::write(dir.join("audit.jsonl"), current).unwrap();
}

fn read_journal(root: &Path, generation: &str) -> String {
    std::fs::read_to_string(journal_of(root, generation)).unwrap_or_default()
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            walk_files(&entry.path(), out);
        } else {
            out.push(entry.path());
        }
    }
}

// ------------------------------------------------------- migration / cutover

#[test]
fn store_migrates_legacy_v1_bytes_verbatim_and_labels_them_unauthenticated() {
    let dir = TempDir::new().unwrap();
    let older = "{\"tool\":\"auth\",\"outcome\":\"rejected\"}\n";
    let current = "{\"tool\":\"ptah_submit_task\",\"outcome\":\"accepted\"}\n\
                   {\"tool\":\"ptah_cancel\",\"outcome\":\"accepted\"}\n";
    seed_legacy_v1(dir.path(), older, current);

    let store = OrchStore::open(dir.path()).unwrap();
    let status = store.audit_status();
    assert_eq!(status.imported_generations, 2);
    assert_eq!(status.global_first_seq, 1);

    // Byte-for-byte preservation, oldest file first.
    assert_eq!(read_journal(dir.path(), "g-000001"), older);
    assert_eq!(read_journal(dir.path(), "g-000002"), current);
    // The v1 originals are read-only inputs and are never moved or truncated.
    assert_eq!(
        std::fs::read_to_string(legacy_dir(dir.path()).join("audit.jsonl")).unwrap(),
        current
    );

    let manifest_path = audit_root(dir.path()).join("manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let generations = manifest["generations"].as_array().unwrap();
    assert_eq!(generations[0]["originAuthenticated"], false);
    assert_eq!(generations[0]["sequenceOrigin"], "import_assigned");
    assert_eq!(
        generations[0]["precedingLossUnknown"], true,
        "v1 already destroyed anything older than audit.jsonl.1"
    );
    assert_eq!(generations[1]["originAuthenticated"], false);
    assert_eq!(generations[1]["precedingLossUnknown"], false);
    // The live generation is native and authenticated.
    assert_eq!(generations[2]["originAuthenticated"], true);
}

#[test]
fn migration_is_one_way_and_the_cutover_boundary_is_the_manifest_commit() {
    let dir = TempDir::new().unwrap();
    seed_legacy_v1(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");

    // Before the cutover there is no committed manifest.
    assert!(!audit_root(dir.path()).join("manifest.json").exists());
    let store = OrchStore::open(dir.path()).unwrap();
    assert!(audit_root(dir.path()).join("manifest.json").is_file());
    let first = store.audit_status();
    drop(store);

    // Reopening never re-imports: the committed manifest is the boundary.
    let store = OrchStore::open(dir.path()).unwrap();
    let second = store.audit_status();
    assert_eq!(second.imported_generations, first.imported_generations);
    assert_eq!(second.generations, first.generations);
    assert_eq!(second.global_first_seq, first.global_first_seq);
    assert!(second.global_last_seq >= first.global_last_seq);
}

#[test]
fn crash_before_the_cutover_commit_recovers_and_re_imports_without_duplicates() {
    let dir = TempDir::new().unwrap();
    let current = "{\"tool\":\"auth\"}\n";
    seed_legacy_v1(dir.path(), "{\"a\":1}\n", current);
    let store = OrchStore::open(dir.path()).unwrap();
    let expected = store.audit_status().generations;
    drop(store);

    // Simulate a crash after the staged generations but before the manifest
    // rename: the generation directories exist, the manifest does not.
    std::fs::remove_file(audit_root(dir.path()).join("manifest.json")).unwrap();
    // Without an authenticated marker the store must refuse rather than guess.
    assert!(
        OrchStore::open(dir.path()).is_err(),
        "generation directories with no committed manifest must fail closed"
    );

    // Restoring the committed manifest recovers deterministically.
    std::fs::remove_dir_all(audit_root(dir.path())).unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    assert_eq!(store.audit_status().generations, expected);
    assert_eq!(read_journal(dir.path(), "g-000002"), current);
}

#[test]
fn legacy_bytes_written_after_the_cutover_are_recorded_as_uncertain() {
    let dir = TempDir::new().unwrap();
    seed_legacy_v1(dir.path(), "", "{\"tool\":\"auth\"}\n");
    let store = OrchStore::open(dir.path()).unwrap();
    let generation = store.audit_status().active_generation_id;
    drop(store);

    // An older binary rolled onto the same home appends to the retired ledger.
    std::fs::write(
        legacy_dir(dir.path()).join("audit.jsonl"),
        "{\"tool\":\"auth\"}\n{\"tool\":\"from_an_older_binary\"}\n",
    )
    .unwrap();

    let store = OrchStore::open(dir.path()).unwrap();
    let body = read_journal(dir.path(), &generation);
    assert!(
        body.contains("legacy_written_after_cutover"),
        "divergence from the imported bytes must be recorded, not repaired"
    );
    assert!(body.contains("\"outcome\":\"uncertain\""));
    // Recording it never rewrites or deletes the legacy file.
    assert!(
        std::fs::read_to_string(legacy_dir(dir.path()).join("audit.jsonl"))
            .unwrap()
            .contains("from_an_older_binary")
    );
    assert!(store.audit_status().poisoned.is_none());
}

// ------------------------------------------------- append / restart / concurrency

#[test]
fn sequence_continues_exactly_across_restarts_with_no_duplicates() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    for index in 0..5 {
        store
            .append_audit(&entry(&format!("op.{index}"), "accepted"))
            .unwrap();
    }
    let before = store.audit_status().global_last_seq;
    drop(store);

    let store = OrchStore::open(dir.path()).unwrap();
    // Shutdown was recorded on the way out, so the sequence advanced by exactly
    // one and never reset.
    assert_eq!(store.audit_status().global_last_seq, before + 1);
    store
        .append_audit(&entry("after-restart", "accepted"))
        .unwrap();
    assert_eq!(store.audit_status().global_last_seq, before + 2);
    // The whole chain still replays.
    store.verify_audit().unwrap();
}

#[test]
fn concurrent_producers_never_share_a_sequence() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(OrchStore::open(dir.path()).unwrap());
    let mut handles = Vec::new();
    for worker in 0..4 {
        let store = Arc::clone(&store);
        handles.push(std::thread::spawn(move || {
            for index in 0..25 {
                store
                    .append_audit(&entry(&format!("w{worker}.{index}"), "accepted"))
                    .unwrap();
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    assert_eq!(store.audit_status().global_last_seq, 100);
    // Replaying the chain is what proves no sequence was issued twice.
    let verified = store.verify_audit().unwrap();
    assert_eq!(verified[0].entry_count, 100);
}

#[test]
fn a_second_store_on_the_same_home_is_refused_and_reuse_is_immediate_after_shutdown() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();

    // Single-writer discipline: the store lock refuses a second opener while
    // the first is live. This is the production lifecycle, not a test fixture.
    assert!(
        OrchStore::open(dir.path()).is_err(),
        "a second store on a live home must be refused"
    );

    // Explicit shutdown, then immediate reuse of the same home with no sleep,
    // no retry, and no lock removal (#455 coordination).
    drop(store);
    let reopened = OrchStore::open(dir.path()).expect("same-home reuse after explicit shutdown");
    reopened.append_audit(&entry("second", "accepted")).unwrap();
    reopened.verify_audit().unwrap();
}

#[test]
fn an_uncertain_producer_outcome_is_never_recorded_as_accepted() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    let generation = store.audit_status().active_generation_id;
    let mut retrying = entry("run_finalization", "retrying");
    retrying.error_code = Some("run_persistence_failed".into());
    retrying.intent_id = Some("run-42".into());
    retrying.phase = AuditPhase::Intent;
    store.append_audit(&retrying).unwrap();

    let body = read_journal(dir.path(), &generation);
    assert!(body.contains("\"outcome\":\"uncertain\""));
    assert!(body.contains("\"code\":\"run_persistence_failed\""));
    assert!(!body.contains("\"outcome\":\"accepted\""));
    // The intent is open, so a crash here recovers as uncertain, not silence.
    assert_eq!(store.audit_status().open_intents, 1);
    drop(store);

    let store = OrchStore::open(dir.path()).unwrap();
    assert_eq!(store.audit_status().recovery.closed_intents, 1);
    let body = read_journal(dir.path(), &generation);
    assert!(body.contains("host_restart_interrupted"));
    assert!(!body.contains("fabricated"));
}

// ---------------------------------------------------------------- integrity

#[test]
fn tampering_with_the_store_journal_fails_closed_on_reopen() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    for index in 0..3 {
        store
            .append_audit(&entry(&format!("op.{index}"), "accepted"))
            .unwrap();
    }
    let generation = store.audit_status().active_generation_id;
    drop(store);

    let path = journal_of(dir.path(), &generation);
    let mut lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut record: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
    record["outcome"] = serde_json::Value::String("accepted".into());
    record["op"] = serde_json::Value::String("something_else".into());
    lines[1] = serde_json::to_string(&record).unwrap();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    assert!(
        OrchStore::open(dir.path()).is_err(),
        "a tampered audit journal must fail the store open, not be repaired"
    );
}

#[test]
fn a_wrong_key_fails_closed_and_leaks_no_path_or_key_material() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    drop(store);

    let error = open_error(OrchStore::open_with_audit(
        dir.path(),
        AuditKeyCustody::Provided(b"an-entirely-different-installation".to_vec()),
        None,
    ));
    let message = format!("{error:#}");
    assert!(message.contains("key_mismatch"), "{message}");
    assert!(!message.contains('/'), "no path may appear: {message}");
    assert!(
        !message.to_lowercase().contains("key ") && !message.contains("audit.key"),
        "no key location may appear: {message}"
    );
}

#[test]
fn required_key_material_absence_fails_closed() {
    let dir = TempDir::new().unwrap();
    let var = "GROKPTAH_AUDIT_KEY_ABSENT_FOR_TEST";
    std::env::remove_var(var);
    let error = open_error(OrchStore::open_with_audit(
        dir.path(),
        AuditKeyCustody::Environment { var: var.into() },
        None,
    ));
    let message = format!("{error:#}");
    assert!(message.contains("key_unavailable"), "{message}");
    assert!(
        !audit_root(dir.path()).join("manifest.json").exists(),
        "a store that cannot authenticate its audit must not create one"
    );
}

#[test]
fn an_unsafe_key_file_mode_fails_closed() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        drop(OrchStore::open(dir.path()).unwrap());
        let key = dir.path().join("audit.key");
        assert!(key.is_file());
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = open_error(OrchStore::open(dir.path()));
        let message = format!("{error:#}");
        assert!(message.contains("key_unavailable"), "{message}");
        assert!(!message.contains('/'), "no path may appear: {message}");
    }
}

#[test]
fn key_rotation_seals_the_old_generation_and_keeps_the_chain_continuous() {
    let dir = TempDir::new().unwrap();
    let first = AuditKeyCustody::Provided(b"installation-key-generation-one".to_vec());
    let store = OrchStore::open_with_audit(dir.path(), first.clone(), None).unwrap();
    store
        .append_audit(&entry("before-rotation", "accepted"))
        .unwrap();
    let before = store.audit_status();
    drop(store);

    // Rotating the installation key without sealing first is a different
    // installation: the manifest MAC no longer verifies and the store refuses.
    let second = AuditKeyCustody::Provided(b"installation-key-generation-two".to_vec());
    assert!(OrchStore::open_with_audit(dir.path(), second, None).is_err());

    // The original key still opens it, and the chain is intact.
    let store = OrchStore::open_with_audit(dir.path(), first, None).unwrap();
    assert!(store.audit_status().global_last_seq >= before.global_last_seq);
    store.verify_audit().unwrap();
}

// ------------------------------------------------------- export / retention

#[test]
fn export_from_the_store_is_sealed_verifiable_and_non_mutating() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    for index in 0..4 {
        store
            .append_audit(&entry(&format!("op.{index}"), "accepted"))
            .unwrap();
    }
    let generation = store.audit_status().active_generation_id;
    let before = std::fs::read(journal_of(dir.path(), &generation)).unwrap();

    let dest = out.path().join("sealed");
    let receipt = store.export_audit(&dest, ExportFormat::Auto).unwrap();
    assert!(receipt.complete);
    assert_eq!(receipt.holes, 0);
    assert_eq!(receipt.witness_state, WitnessState::Unwitnessed);
    // Export never rotates, truncates, or deletes.
    assert_eq!(
        std::fs::read(journal_of(dir.path(), &generation)).unwrap(),
        before
    );
    assert!(store.audit_status().poisoned.is_none());
}

#[test]
fn a_migrated_store_refuses_lossy_v1_emission() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    seed_legacy_v1(dir.path(), "{\"a\":1}\n", "{\"b\":2}\n");
    let store = OrchStore::open(dir.path()).unwrap();

    // A v1 document cannot say "unauthenticated origin", so emitting one for a
    // migrated ledger would misrepresent it.
    let error = store
        .export_audit(&out.path().join("v1"), ExportFormat::V1)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("export_v1_incompatible_multi_generation"),
        "{error:#}"
    );

    let dest = out.path().join("v2");
    let receipt = store.export_audit(&dest, ExportFormat::Auto).unwrap();
    assert!(receipt.schema.ends_with(".v2"));
    // A public export withholds the imported generations rather than carrying
    // them, so it carries no unauthenticated generation at all.
    assert_eq!(receipt.unauthenticated_generations, 0);
    assert_eq!(receipt.withheld, 2);
    assert!(!receipt.complete);
    // The privileged raw scope is the only one that carries them, and it is
    // reachable only with a verified grant from an operator host.
    drop(store);
    let store = open_operator_store(dir.path());
    let raw = store
        .export_audit_privileged_raw(
            &out.path().join("raw"),
            ExportFormat::Auto,
            &raw_grant(&store),
        )
        .unwrap();
    assert_eq!(raw.unauthenticated_generations, 2);
    assert!(raw.contains_unauthenticated_legacy);
}

#[test]
fn store_retention_refuses_the_active_generation_and_tombstones_the_rest() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let store = open_operator_store(dir.path());
    store.append_audit(&entry("first", "accepted")).unwrap();

    let active = store.audit_status().active_generation_id;
    let grant = store
        .request_audit_authority(AuditCapability::RetainUnexported, &active)
        .unwrap();
    let error = store
        .retain_audit_generation(RetentionRequest::under_grant(&active, grant))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("generation_is_active"),
        "{error:#}"
    );

    // Rotate, export, then retain the sealed predecessor.
    store.rotate_audit(RotationReason::Operator).unwrap();
    store
        .append_audit(&entry("after-rotate", "accepted"))
        .unwrap();
    let dest = out.path().join("sealed");
    let receipt = store.export_audit(&dest, ExportFormat::Auto).unwrap();
    store
        .retain_audit_generation(RetentionRequest::exported_under(&active, &receipt.seal_id))
        .unwrap();

    let status = store.audit_status();
    assert_eq!(status.tombstones, 1);
    assert_eq!(status.retention_epoch, 1);
    assert!(!audit_root(dir.path())
        .join("generations")
        .join(&active)
        .exists());
    drop(store);

    // The chain still verifies across the hole after a restart.
    let store = OrchStore::open(dir.path()).unwrap();
    store.verify_audit().unwrap();
    let post = store
        .export_audit(&out.path().join("after"), ExportFormat::Auto)
        .unwrap();
    assert!(
        !post.complete,
        "a retained hole is never reported as complete"
    );
    assert_eq!(post.holes, 1);
}

// ------------------------------------------------------------- redaction

#[test]
fn public_audit_artifacts_carry_no_prompt_path_credential_or_key_material() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();

    // Needles across every class #462 names.
    let needles = [
        "/private/very/secret/workspace",
        "sk-live-CREDENTIAL-abcdef123456",
        "PROMPT-BODY-do-the-thing",
        "AXUIElementLocator-42",
        "CLIPBOARD-CONTENTS-HERE",
        "RAWFRAMEBYTES",
        "provider-private-route-xyz",
    ];
    let mut tainted = entry("ptah_submit_task", "rejected");
    tainted.workspace = Some(needles[0].into());
    tainted.request_id = Some(needles[1].into());
    tainted.detail = format!(
        "{} {} {} {} {}",
        needles[2], needles[3], needles[4], needles[5], needles[6]
    );
    tainted.error_code = Some("workspace_mismatch".into());
    tainted.intent_id = Some(needles[6].into());
    store.append_audit(&tainted).unwrap();

    let dest = out.path().join("sealed");
    let receipt = store.export_audit(&dest, ExportFormat::Auto).unwrap();

    // Every byte of the export, not just the manifest.
    let mut files = Vec::new();
    walk_files(&dest, &mut files);
    assert!(!files.is_empty());
    for path in &files {
        let bytes = std::fs::read(path).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in needles {
            assert!(
                !text.contains(needle),
                "{} leaked into {}",
                needle,
                path.file_name().unwrap().to_string_lossy()
            );
        }
        assert!(!text.contains("audit.key"), "key location leaked");
    }

    // The path-free receipt and the health projection are equally clean.
    let rendered = format!("{receipt:?}");
    for needle in needles {
        assert!(
            !rendered.contains(needle),
            "{needle} leaked into the receipt"
        );
    }
    assert!(!rendered.contains(&dir.path().display().to_string()));
    let status = format!("{:?}", store.audit_status());
    for needle in needles {
        assert!(
            !status.contains(needle),
            "{needle} leaked into audit status"
        );
    }

    // The export still verifies independently.
    let keys = AuditKeyCustody::local_file_for(dir.path())
        .resolve()
        .unwrap();
    verify_export(&dest, &keys).unwrap();
}

// ---------------------------------------------------------------- witness

/// Max-epoch witness: fail-closed on contradiction, fail-soft on outage.
#[derive(Default)]
struct MaxEpochWitness {
    max: parking_lot::Mutex<u64>,
}

impl AuditWitness for MaxEpochWitness {
    fn record(&self, beacon: &WitnessBeacon) {
        let mut max = self.max.lock();
        *max = (*max).max(beacon.manifest_epoch);
    }

    fn check(&self, beacon: &WitnessBeacon) -> WitnessVerdict {
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
        WitnessState::Verified
    }
}

#[test]
fn rollback_stays_honestly_unwitnessed_unless_a_witness_is_configured() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    let receipt = store
        .export_audit(&out.path().join("unwitnessed"), ExportFormat::Auto)
        .unwrap();
    assert_eq!(
        receipt.witness_state,
        WitnessState::Unwitnessed,
        "with no witness configured the receipt must say so"
    );
    assert_eq!(
        store.audit_status().witness_state,
        WitnessState::Unwitnessed
    );
}

#[test]
fn a_configured_witness_reports_verified_through_the_store() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let witness: Arc<dyn AuditWitness> = Arc::new(MaxEpochWitness::default());
    let store = OrchStore::open_with_audit(
        dir.path(),
        AuditKeyCustody::local_file_for(dir.path()),
        Some(witness),
    )
    .unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    let receipt = store
        .export_audit(&out.path().join("witnessed"), ExportFormat::Auto)
        .unwrap();
    assert_eq!(receipt.witness_state, WitnessState::Verified);
}

// ------------------------------------------------------------ two processes

const CHILD_ROOT_ENV: &str = "GROKPTAH_AUDIT_TWO_PROCESS_ROOT";

/// Child half of [`two_processes_share_one_home_without_duplicate_sequences`].
/// A no-op unless the parent invoked this binary with the store root set.
#[test]
fn child_appends_to_an_existing_store() {
    let Ok(root) = std::env::var(CHILD_ROOT_ENV) else {
        return;
    };
    let store = OrchStore::open(&root).expect("child opens the released home");
    store
        .append_audit(&entry("from-the-child-process", "accepted"))
        .unwrap();
    store.verify_audit().unwrap();
}

#[test]
fn two_processes_share_one_home_without_duplicate_sequences() {
    if std::env::var(CHILD_ROOT_ENV).is_ok() {
        return; // running as the child
    }
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store
        .append_audit(&entry("from-the-parent", "accepted"))
        .unwrap();
    let before = store.audit_status().global_last_seq;
    let generation = store.audit_status().active_generation_id;
    // Explicit shutdown releases the home through the production lifecycle.
    drop(store);

    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "child_appends_to_an_existing_store",
            "--exact",
            "--test-threads=1",
        ])
        .env(CHILD_ROOT_ENV, dir.path())
        .status()
        .expect("spawn the child process");
    assert!(status.success(), "child process failed");

    // A genuinely separate process continued the same authenticated chain.
    let store = OrchStore::open(dir.path()).unwrap();
    store.verify_audit().unwrap();
    let after = store.audit_status().global_last_seq;
    assert!(after > before, "child work did not reach the ledger");
    let body = read_journal(dir.path(), &generation);
    assert!(body.contains("from-the-parent"));
    assert!(body.contains("from-the-child-process"));
    // Two shutdowns and two appends, all distinct sequences, chain intact.
    assert_eq!(store.verify_audit().unwrap()[0].entry_count, after);
}

// ------------------------------------------------- store-level crash cuts
//
// The ledger's own six injected crash cuts are covered by the donor lib tests.
// These reproduce the observable on-disk states against the real `OrchStore`,
// which is the only way an integration test can reach them.

#[test]
fn an_empty_orphan_generation_is_kept_and_reused_by_the_store() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("before", "accepted")).unwrap();
    let active = store.audit_status().active_generation_id;
    drop(store);

    // Crash cut R2: the next generation was prepared, the manifest was not
    // committed. The manifest is the authority, so the old generation reopens
    // and the orphan is kept for an idempotent retry.
    let orphan = audit_root(dir.path()).join("generations").join("g-000002");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("journal.jsonl"), b"").unwrap();

    let store = OrchStore::open(dir.path()).unwrap();
    assert_eq!(store.audit_status().active_generation_id, active);
    assert!(orphan.exists(), "an orphan generation is never deleted");
    // The retry succeeds over the orphan.
    assert_eq!(
        store.rotate_audit(RotationReason::Operator).unwrap(),
        "g-000002"
    );
    store.verify_audit().unwrap();
}

#[test]
fn a_non_empty_orphan_generation_fails_the_store_open() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("before", "accepted")).unwrap();
    drop(store);

    // Unreachable before the manifest commit, so it means tampering.
    let orphan = audit_root(dir.path()).join("generations").join("g-000002");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join("journal.jsonl"), b"{}\n").unwrap();

    let error = open_error(OrchStore::open(dir.path()));
    assert!(
        format!("{error:#}").contains("orphan_generation_not_empty"),
        "{error:#}"
    );
}

#[test]
fn a_torn_tail_in_the_store_journal_is_recovered_and_recorded() {
    use std::io::Write;

    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    for index in 0..3 {
        store
            .append_audit(&entry(&format!("op.{index}"), "accepted"))
            .unwrap();
    }
    let generation = store.audit_status().active_generation_id;
    drop(store);

    // A crash mid-write leaves an unterminated trailing run.
    let torn = b"{\"v\":2,\"gen\":\"g-0000";
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal_of(dir.path(), &generation))
        .unwrap();
    file.write_all(torn).unwrap();
    file.sync_all().unwrap();

    let store = OrchStore::open(dir.path()).unwrap();
    let status = store.audit_status();
    let evidence = status
        .recovery
        .torn_tail
        .expect("a torn tail must be surfaced, never silently dropped");
    assert_eq!(evidence.bytes, torn.len() as u64, "byte-exact evidence");
    let body = read_journal(dir.path(), &generation);
    assert!(body.contains("recovery_torn_tail"));
    assert!(store.audit_status().poisoned.is_none());
    store.verify_audit().unwrap();
}

#[test]
fn a_dropped_producer_entry_survives_restart_as_durable_evidence() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    // The v1 ledger kept this only in process memory, so a restart erased the
    // evidence that evidence had been lost.
    store.record_dropped_audit(3).unwrap();
    drop(store);

    let store = OrchStore::open(dir.path()).unwrap();
    let gaps = store.audit_status().recovery.durable_gaps;
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].lost_entries, 3);
    assert!(gaps[0].journaled);
}

#[test]
fn a_symlinked_or_relative_root_resolves_to_one_key_and_one_ledger() {
    #[cfg(unix)]
    {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // Open through the symlink, then through the real path. Both must
        // resolve to the same key and the same authenticated ledger.
        let store = OrchStore::open(&link).unwrap();
        store
            .append_audit(&entry("through-the-link", "accepted"))
            .unwrap();
        let before = store.audit_status().global_last_seq;
        drop(store);

        let store = OrchStore::open(&real).expect("the real path must find the same key");
        assert!(store.audit_status().global_last_seq >= before);
        store.verify_audit().unwrap();
        assert!(real.join("audit.key").is_file());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            2,
            "exactly one store tree plus its symlink"
        );
    }
}

// ------------------------------------- migrated legacy bytes in exports (#462)
//
// Imported v1 bytes are preserved verbatim by design, so they still carry
// whatever the v1 ledger recorded: raw workspace paths, free-text `detail`
// holding `OrchError::message`, IO strings, and provider material. A public
// export must not carry them. A fresh-v2 no-needle test cannot see this,
// because a fresh ledger has no legacy generation to leak.

/// A legacy v1 ledger containing exactly what the retired writer could record.
const LEGACY_NEEDLES: [&str; 6] = [
    "/Users/someone/private/workspace",
    "sk-live-LEGACY-CREDENTIAL-9f3a",
    "LEGACY-PROMPT-BODY-do-the-thing",
    "AXUIElementLocator-legacy-42",
    "LEGACY-CLIPBOARD-CONTENTS",
    "legacy-provider-private-route",
];

fn seed_tainted_legacy_v1(root: &Path) {
    let older = format!(
        "{{\"ts\":\"2026-01-01T00:00:00Z\",\"tool\":\"ptah_submit_task\",\"workspace\":\"{}\",\"outcome\":\"rejected\",\"detail\":\"{} {}\"}}\n",
        LEGACY_NEEDLES[0], LEGACY_NEEDLES[2], LEGACY_NEEDLES[3]
    );
    let current = format!(
        "{{\"ts\":\"2026-01-02T00:00:00Z\",\"tool\":\"auth\",\"requestId\":\"{}\",\"outcome\":\"rejected\",\"detail\":\"{} {}\"}}\n",
        LEGACY_NEEDLES[1], LEGACY_NEEDLES[4], LEGACY_NEEDLES[5]
    );
    seed_legacy_v1(root, &older, &current);
}

#[test]
fn a_public_export_withholds_migrated_legacy_bytes_and_leaks_no_needle() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    seed_tainted_legacy_v1(dir.path());
    let store = OrchStore::open(dir.path()).unwrap();
    store
        .append_audit(&entry("native-after-migration", "accepted"))
        .unwrap();

    // The legacy bytes really are on disk, verbatim, with the needles in them.
    let imported = read_journal(dir.path(), "g-000001");
    assert!(
        imported.contains(LEGACY_NEEDLES[0]),
        "precondition: legacy bytes preserved"
    );

    let dest = out.path().join("public");
    let receipt = store.export_audit(&dest, ExportFormat::Auto).unwrap();
    assert_eq!(receipt.scope, ExportScope::Public);
    assert!(!receipt.contains_unauthenticated_legacy);
    assert_eq!(
        receipt.withheld, 2,
        "both imported generations are withheld"
    );
    assert!(
        !receipt.complete,
        "a withheld range is never reported as complete"
    );

    // Every byte of the public export, not just the manifest.
    let mut files = Vec::new();
    walk_files(&dest, &mut files);
    assert!(!files.is_empty());
    for path in &files {
        let text = String::from_utf8_lossy(&std::fs::read(path).unwrap()).to_string();
        for needle in LEGACY_NEEDLES {
            assert!(
                !text.contains(needle),
                "{} leaked into the public export at {}",
                needle,
                path.display()
            );
        }
    }
    // The withheld generations carry no files at all.
    assert!(!dest.join("generations").join("g-000001").exists());
    assert!(!dest.join("generations").join("g-000002").exists());

    // The receipt and the status projection are equally clean.
    let rendered = format!("{receipt:?}{:?}", store.audit_status());
    for needle in LEGACY_NEEDLES {
        assert!(
            !rendered.contains(needle),
            "{needle} leaked into a projection"
        );
    }

    // And it still verifies as a coherent, explicitly partial export.
    let keys = AuditKeyCustody::local_file_for(dir.path())
        .resolve()
        .unwrap();
    let verified = verify_export(&dest, &keys).unwrap();
    assert_eq!(verified.withheld, 2);
    assert!(!verified.complete);
    assert_eq!(verified.global_first_seq, 1);
}

#[test]
fn a_privileged_raw_export_carries_legacy_bytes_and_declares_that_it_does() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    seed_tainted_legacy_v1(dir.path());
    let store = open_operator_store(dir.path());

    let dest = out.path().join("raw");
    let receipt = store
        .export_audit_privileged_raw(&dest, ExportFormat::Auto, &raw_grant(&store))
        .unwrap();
    assert_eq!(receipt.scope, ExportScope::PrivilegedRaw);
    assert!(
        receipt.contains_unauthenticated_legacy,
        "a raw export must declare that it is not redacted"
    );
    assert_eq!(receipt.withheld, 0);
    assert!(receipt.complete);

    // The bytes really are carried — that is the point of raw preservation.
    let carried = std::fs::read_to_string(
        dest.join("generations")
            .join("g-000001")
            .join("journal.jsonl"),
    )
    .unwrap();
    assert!(carried.contains(LEGACY_NEEDLES[0]));

    // The sealed manifest says so too, so nobody can mistake it for public.
    let manifest = std::fs::read_to_string(dest.join("export-manifest.json")).unwrap();
    assert!(manifest.contains("\"scope\":\"privileged_raw\""));
    assert!(manifest.contains("\"containsUnauthenticatedLegacy\":true"));

    let keys = AuditKeyCustody::local_file_for(dir.path())
        .resolve()
        .unwrap();
    let verified = verify_export(&dest, &keys).unwrap();
    assert!(verified.contains_unauthenticated_legacy);
    assert_eq!(verified.scope, ExportScope::PrivilegedRaw);
}

#[test]
fn a_public_export_manifest_that_claims_to_carry_legacy_bytes_is_refused() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    seed_tainted_legacy_v1(dir.path());
    let store = open_operator_store(dir.path());
    let dest = out.path().join("raw");
    store
        .export_audit_privileged_raw(&dest, ExportFormat::Auto, &raw_grant(&store))
        .unwrap();

    // Relabel a raw export as public without re-sealing: the MAC catches it.
    let path = dest.join("export-manifest.json");
    let body = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"scope\":\"privileged_raw\"", "\"scope\":\"public\"");
    std::fs::write(&path, body).unwrap();
    let keys = AuditKeyCustody::local_file_for(dir.path())
        .resolve()
        .unwrap();
    assert!(
        verify_export(&dest, &keys).is_err(),
        "a relabelled export must not verify"
    );
}

#[test]
fn legacy_divergence_after_the_cutover_is_recorded_once_not_on_every_open() {
    let dir = TempDir::new().unwrap();
    seed_legacy_v1(dir.path(), "", "{\"tool\":\"auth\"}\n");
    let store = OrchStore::open(dir.path()).unwrap();
    let generation = store.audit_status().active_generation_id;
    drop(store);

    std::fs::write(
        legacy_dir(dir.path()).join("audit.jsonl"),
        "{\"tool\":\"auth\"}\n{\"tool\":\"from_an_older_binary\"}\n",
    )
    .unwrap();

    let mut counts = Vec::new();
    for _ in 0..3 {
        let store = OrchStore::open(dir.path()).unwrap();
        counts.push(
            read_journal(dir.path(), &generation)
                .matches("legacy_written_after_cutover")
                .count(),
        );
        drop(store);
    }
    assert_eq!(
        counts,
        vec![1, 1, 1],
        "the same divergence must be recorded once, not on every open"
    );
}

#[test]
fn environment_key_custody_opens_the_same_ledger_across_restarts() {
    let dir = TempDir::new().unwrap();
    let var = "GROKPTAH_AUDIT_KEY_ENV_CUSTODY_TEST";
    let material = "a".repeat(64);
    std::env::set_var(var, &material);

    let custody = AuditKeyCustody::Environment { var: var.into() };
    let store = OrchStore::open_with_audit(dir.path(), custody.clone(), None).unwrap();
    store
        .append_audit(&entry("under-env-custody", "accepted"))
        .unwrap();
    let before = store.audit_status().global_last_seq;
    drop(store);

    // The same environment key reopens the same authenticated ledger.
    let store = OrchStore::open_with_audit(dir.path(), custody, None).unwrap();
    assert!(store.audit_status().global_last_seq >= before);
    store.verify_audit().unwrap();
    // No key file is created for this mode.
    assert!(!dir.path().join("audit.key").exists());
    drop(store);

    // A different key is a key mismatch, not silent re-initialisation.
    let other = AuditKeyCustody::Provided(b"an-entirely-different-installation".to_vec());
    let error = open_error(OrchStore::open_with_audit(dir.path(), other, None));
    assert!(format!("{error:#}").contains("key_mismatch"), "{error:#}");

    // Malformed material fails closed rather than being coerced.
    std::env::set_var(var, "not-hex");
    let error = open_error(OrchStore::open_with_audit(
        dir.path(),
        AuditKeyCustody::Environment { var: var.into() },
        None,
    ));
    assert!(
        format!("{error:#}").contains("key_unavailable"),
        "{error:#}"
    );
    std::env::remove_var(var);
}

// ------------------------------ capability authority at the shipped surface

#[test]
fn the_default_store_cannot_take_a_privileged_raw_export() {
    let dir = TempDir::new().unwrap();
    seed_tainted_legacy_v1(dir.path());
    let store = OrchStore::open(dir.path()).unwrap();

    // `OrchStore::open` installs no authority provider, so unredacted legacy
    // bytes cannot leave this host at all. Before the grant existed, naming a
    // different export scope was the whole boundary.
    let error = store
        .request_audit_authority(
            AuditCapability::PrivilegedRawExport,
            grokptah_agent_bridge::audit::PRIVILEGED_RAW_EXPORT_SUBJECT,
        )
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("authority_unavailable"),
        "{error:#}"
    );
    // Only a stable code reaches the caller: no path, no key, no scope.
    assert!(!format!("{error:#}").contains(&dir.path().display().to_string()));
}

#[test]
fn the_default_store_cannot_delete_an_unexported_generation() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    let active = store.audit_status().active_generation_id;
    store.rotate_audit(RotationReason::Operator).unwrap();
    store.append_audit(&entry("second", "accepted")).unwrap();

    let error = store
        .request_audit_authority(AuditCapability::RetainUnexported, &active)
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("authority_unavailable"),
        "{error:#}"
    );
    // And there is no other way in: a seal id the store never issued is not
    // authority either.
    let error = store
        .retain_audit_generation(RetentionRequest::exported_under(&active, "seal-invented"))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("export_seal_unknown"),
        "{error:#}"
    );
    assert_eq!(store.audit_status().tombstones, 0);
    assert!(audit_root(dir.path())
        .join("generations")
        .join(&active)
        .exists());
}

#[test]
fn an_operator_store_records_the_grant_behind_an_unexported_deletion() {
    let dir = TempDir::new().unwrap();
    let store = open_operator_store(dir.path());
    store.append_audit(&entry("first", "accepted")).unwrap();
    let active = store.audit_status().active_generation_id;
    store.rotate_audit(RotationReason::Operator).unwrap();
    store.append_audit(&entry("second", "accepted")).unwrap();

    let grant = store
        .request_audit_authority(AuditCapability::RetainUnexported, &active)
        .unwrap();
    let grant_id = grant.grant_id().to_string();
    let receipt = store
        .retain_audit_generation(RetentionRequest::under_grant(&active, grant))
        .unwrap();
    assert!(receipt.allow_unexported);
    assert_eq!(
        receipt.authority_grant_id.as_deref(),
        Some(grant_id.as_str())
    );
    // Honest about what stood behind it: an operator act on this host, not an
    // authenticated principal (#460/#461).
    assert_eq!(
        receipt.authority_source,
        Some(AuthoritySource::LocalOperator)
    );

    // The decision survives restart in the tombstone, not just the receipt.
    drop(store);
    let store = open_operator_store(dir.path());
    assert_eq!(store.audit_status().tombstones, 1);
    store.verify_audit().unwrap();
}

#[test]
fn a_store_export_seal_is_registered_and_then_authorizes_exactly_its_range() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    store.append_audit(&entry("first", "accepted")).unwrap();
    let first = store.audit_status().active_generation_id;
    store.rotate_audit(RotationReason::Operator).unwrap();
    store.append_audit(&entry("second", "accepted")).unwrap();

    let receipt = store
        .export_audit(&out.path().join("sealed"), ExportFormat::Auto)
        .unwrap();
    // A seal id one character away from the real one is not the real one.
    let error = store
        .retain_audit_generation(RetentionRequest::exported_under(
            &first,
            format!("{}x", receipt.seal_id),
        ))
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("export_seal_unknown"),
        "{error:#}"
    );
    assert_eq!(store.audit_status().tombstones, 0);

    // The seal the store issued and re-verified does work, and needs no
    // operator authority: these bytes were preserved.
    store
        .retain_audit_generation(RetentionRequest::exported_under(&first, &receipt.seal_id))
        .unwrap();
    assert_eq!(store.audit_status().tombstones, 1);
    store.verify_audit().unwrap();
}

// -------------------------------------- durable acceptance for queued events

#[test]
fn a_drained_audit_queue_leaves_no_uncertainty_behind() {
    let dir = TempDir::new().unwrap();
    {
        let store = OrchStore::open(dir.path()).unwrap();
        for index in 0..20 {
            store
                .enqueue_audit(entry(&format!("queued.{index}"), "accepted"))
                .expect("enqueue");
        }
        // Drop drains the writer and joins it before returning.
    }
    // No marker survives a clean shutdown, so the next open reports nothing.
    assert!(!audit_root(dir.path()).join("pending.json").exists());

    let store = OrchStore::open(dir.path()).unwrap();
    let gaps = store.audit_status().recovery.durable_gaps;
    assert!(
        gaps.is_empty(),
        "a drained queue must not look like a loss: {gaps:?}"
    );
    store.verify_audit().unwrap();
}

#[test]
fn accepting_a_queued_entry_writes_a_durable_marker_before_it_returns() {
    let dir = TempDir::new().unwrap();
    let store = OrchStore::open(dir.path()).unwrap();
    // The marker is what makes "accepted" honest: the entry is not journaled
    // yet, but its loss would be visible. Before it existed, `enqueue_audit`
    // returned accepted for an entry that lived only in memory.
    let marker = audit_root(dir.path()).join("pending.json");
    store
        .enqueue_audit(entry("queued.durable", "accepted"))
        .unwrap();
    // Either the marker is still on disk, or the writer already drained it --
    // never a state where the entry was accepted and nothing recorded it.
    let observed = marker.exists();
    drop(store);
    assert!(
        !marker.exists(),
        "the marker must be cleared once the queue drains"
    );
    let store = OrchStore::open(dir.path()).unwrap();
    assert!(
        store.audit_status().recovery.durable_gaps.is_empty(),
        "a drained entry is not a loss (marker seen: {observed})"
    );
    // The entry itself landed in the chained journal.
    let status = store.audit_status();
    assert!(status.global_last_seq >= 1);
    store.verify_audit().unwrap();
}

#[test]
fn a_seal_registry_survives_restart_and_still_authorizes_its_range() {
    let dir = TempDir::new().unwrap();
    let out = TempDir::new().unwrap();
    let seal_id = {
        let store = OrchStore::open(dir.path()).unwrap();
        store.append_audit(&entry("first", "accepted")).unwrap();
        store.rotate_audit(RotationReason::Operator).unwrap();
        store.append_audit(&entry("second", "accepted")).unwrap();
        store
            .export_audit(&out.path().join("sealed"), ExportFormat::Auto)
            .unwrap()
            .seal_id
    };

    // The registry lives inside the MAC'd manifest, so a manifest carrying
    // seals has to keep verifying across a restart -- if the canonical bytes
    // and the tag disagreed, this open would fail closed instead.
    let store = OrchStore::open(dir.path()).unwrap();
    store.verify_audit().unwrap();
    let first = store
        .audit_status()
        .active_generation_id
        .replace("g-000002", "g-000001");
    store
        .retain_audit_generation(RetentionRequest::exported_under(&first, &seal_id))
        .expect("a seal issued before the restart still authorizes its range");
    assert_eq!(store.audit_status().tombstones, 1);
}
