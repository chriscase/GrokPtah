use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use grokptah_agent_bridge::audit::{
    AuditError, AuditKeyCustody, AuditKeyProvider, AuditKeys, AuditResult, ExportFormat,
    PoisonReason, RetentionRequest,
};
use grokptah_agent_bridge::{
    ComputerStore, ComputerUseLimits, ComputerUseService, OrchStore, SimulatorBackend,
};
use tempfile::TempDir;
use uuid::Uuid;

#[derive(Debug)]
struct ExternalKeyProvider {
    key: Arc<AuditKeys>,
}

impl AuditKeyProvider for ExternalKeyProvider {
    fn keyring(&self) -> Vec<Arc<AuditKeys>> {
        vec![Arc::clone(&self.key)]
    }

    fn rotate(&self, _current: &AuditKeys) -> AuditResult<Arc<AuditKeys>> {
        Err(AuditError::Poisoned(PoisonReason::KeyUnavailable))
    }
}

fn all_bytes(root: &Path) -> Vec<u8> {
    fn visit(path: &Path, output: &mut Vec<u8>) {
        let Ok(metadata) = fs::symlink_metadata(path) else {
            return;
        };
        if metadata.is_file() {
            if let Ok(bytes) = fs::read(path) {
                output.extend_from_slice(&bytes);
            }
        } else if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(path) else {
                return;
            };
            for entry in entries.flatten() {
                visit(&entry.path(), output);
            }
        }
    }

    let mut output = Vec::new();
    visit(root, &mut output);
    output
}

fn audited_service(root: &Path) -> (Arc<ComputerUseService>, OrchStore) {
    let audit = OrchStore::open(root.join("orchestration")).unwrap();
    let computer = ComputerStore::open(root.join("computer-use")).unwrap();
    let service = ComputerUseService::new_with_audit_store(
        Arc::new(SimulatorBackend::new()),
        computer,
        audit.clone(),
    );
    (Arc::new(service), audit)
}

#[test]
fn real_store_public_projection_contains_only_bounded_audit_facts() {
    let temp = TempDir::new().unwrap();
    let (service, audit) = audited_service(temp.path());
    service
        .create_run(
            "public-projection",
            Uuid::new_v4(),
            None,
            SimulatorBackend::demo_target(),
            ComputerUseLimits::default(),
        )
        .unwrap();
    let destination = temp.path().join("public-export");
    audit
        .export_audit(&destination, ExportFormat::Auto)
        .unwrap();
    let exported = all_bytes(&destination);
    for forbidden in [
        "prompt",
        "credential",
        "/private/",
        "locator",
        "clipboard",
        "frame",
        "hmac",
        "provider-private-payload",
    ] {
        assert!(
            !exported
                .windows(forbidden.len())
                .any(|window| window == forbidden.as_bytes()),
            "public projection leaked {forbidden}"
        );
    }
}

#[test]
fn real_store_migrates_legacy_bytes_once_and_withholds_them_publicly() {
    let temp = TempDir::new().unwrap();
    let audit_dir = temp.path().join("audit");
    fs::create_dir_all(&audit_dir).unwrap();
    let older = b"{\"detail\":\"/private/path prompt credential clipboard locator\"}\n";
    let current = b"{\"detail\":\"provider-private-payload\"}\n";
    fs::write(audit_dir.join("audit.jsonl.1"), older).unwrap();
    fs::write(audit_dir.join("audit.jsonl"), current).unwrap();

    let store = OrchStore::open(temp.path()).unwrap();
    assert_eq!(store.audit_status().imported_generations, 2);
    assert_eq!(fs::read(audit_dir.join("audit.jsonl.1")).unwrap(), older);
    assert_eq!(fs::read(audit_dir.join("audit.jsonl")).unwrap(), current);
    let destination = temp.path().join("public");
    let receipt = store
        .export_audit(&destination, ExportFormat::Auto)
        .unwrap();
    assert_eq!(receipt.withheld_generations, 2);
    let public_bytes = all_bytes(&destination);
    assert!(!public_bytes
        .windows("/private/path".len())
        .any(|window| window == b"/private/path"));
    assert!(!public_bytes
        .windows("provider-private-payload".len())
        .any(|window| window == b"provider-private-payload"));
}

#[test]
fn real_store_sealed_tamper_fails_closed_at_open() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store.rotate_audit().unwrap();
    store.shutdown().unwrap();
    let journal = temp.path().join("audit/generations/g-000001/journal.jsonl");
    let mut bytes = fs::read(&journal).unwrap();
    bytes[0] = b'X';
    fs::write(journal, bytes).unwrap();
    assert!(OrchStore::open(temp.path()).is_err());
}

#[test]
fn real_store_retention_requires_verified_export_and_commits_tombstone() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store.rotate_audit_key().unwrap();
    let destination = temp.path().join("retention-export");
    let receipt = store.export_audit(&destination, ExportFormat::V2).unwrap();
    assert!(store
        .retain_audit(RetentionRequest::new("g-000001").with_export_seal("bogus"))
        .is_err());
    store
        .retain_audit(RetentionRequest::new("g-000001").with_export_seal(receipt.seal_id))
        .unwrap();
    assert_eq!(store.audit_status().tombstones, 1);
    store.shutdown().unwrap();
    let reopened = OrchStore::open(temp.path()).unwrap();
    assert_eq!(reopened.audit_status().tombstones, 1);
    assert!(reopened.verify_audit().is_ok());
}

#[test]
fn real_store_concurrent_computer_mutations_preserve_exact_sequence() {
    let temp = TempDir::new().unwrap();
    let (service, audit) = audited_service(temp.path());
    let mut workers = Vec::new();
    for index in 0..100 {
        let service = Arc::clone(&service);
        workers.push(thread::spawn(move || {
            service
                .create_run(
                    &format!("concurrent-{index}"),
                    Uuid::new_v4(),
                    None,
                    SimulatorBackend::demo_target(),
                    ComputerUseLimits::default(),
                )
                .unwrap();
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(audit.audit_status().global_last_seq, 200);
    assert_eq!(audit.verify_audit().unwrap(), 1);
}

#[test]
fn two_process_immediate_same_home_reuse_after_explicit_shutdown() {
    if let Ok(path) = std::env::var("GROKPTAH_AUDIT_CHILD_HOME") {
        let store = OrchStore::open(&path).unwrap();
        store.rotate_audit().unwrap();
        store.shutdown().unwrap();
        return;
    }

    let temp = TempDir::new().unwrap();
    let root = PathBuf::from(temp.path());
    let store = OrchStore::open(&root).unwrap();
    store.rotate_audit().unwrap();
    store.shutdown().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("two_process_immediate_same_home_reuse_after_explicit_shutdown")
        .arg("--nocapture")
        .env("GROKPTAH_AUDIT_CHILD_HOME", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let reopened = OrchStore::open(&root).unwrap();
    assert_eq!(reopened.audit_status().global_last_seq, 8);
}

#[test]
fn external_custody_requires_provider_rotation_and_writes_no_epoch_file() {
    let temp = TempDir::new().unwrap();
    let provider = Arc::new(ExternalKeyProvider {
        key: Arc::new(AuditKeys::derive(b"external-custody-key")),
    });
    let custody = AuditKeyCustody::external_consumer(provider).unwrap();
    let store = OrchStore::open_with_custody(temp.path(), custody).unwrap();
    assert!(store.rotate_audit_key().is_err());
    assert!(!temp.path().join(".audit-key-epochs").exists());
    store.shutdown().unwrap();
}

#[test]
fn missing_retired_epoch_fails_closed_after_file_custody_rotation() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store.rotate_audit_key().unwrap();
    store.shutdown().unwrap();
    fs::remove_file(temp.path().join(".audit-key-epochs/epoch-00000002.key")).unwrap();
    assert!(OrchStore::open(temp.path()).is_err());
}
