use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::thread;

use chrono::Utc;
use grokptah_agent_bridge::audit::{AuditKeyCustody, AuditKeys, ExportFormat, RetentionRequest};
use grokptah_agent_bridge::orchestration::AuditEntry;
use grokptah_agent_bridge::OrchStore;
use tempfile::TempDir;

fn audit_entry(tool: &str, request_id: &str, detail: &str) -> AuditEntry {
    AuditEntry {
        ts: Utc::now(),
        tool: tool.to_string(),
        request_id: Some(request_id.to_string()),
        session_id: None,
        workspace: Some("/private/workspace/should-be-keyed".into()),
        outcome: "accepted".into(),
        error_code: None,
        detail: detail.into(),
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
            return;
        }
        if metadata.is_dir() {
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

#[test]
fn real_store_uses_one_authenticated_authority_and_public_projection_is_redacted() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    for (tool, request) in [
        ("orchestration_run", "run-intent"),
        ("provider_attempt", "provider-intent"),
        ("approval", "approval-intent"),
        ("computer_use", "computer-intent"),
        ("queue_background", "queue-intent"),
        ("subagent", "subagent-intent"),
        ("cancellation", "cancel-intent"),
        ("shutdown", "shutdown-intent"),
    ] {
        store
            .append_audit(&audit_entry(
                tool,
                request,
                "super-secret-prompt credential=/private/token clipboard=raw-frame",
            ))
            .unwrap();
    }
    assert_eq!(store.audit_status().global_last_seq, 8);

    let destination = temp.path().join("public-export");
    let receipt = store
        .export_audit(&destination, ExportFormat::Auto)
        .unwrap();
    assert_eq!(receipt.generations_exported, 1);
    let exported = all_bytes(&destination);
    for forbidden in [
        "super-secret-prompt",
        "credential=/private/token",
        "/private/workspace",
        "clipboard=raw-frame",
        "locator",
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
    assert!(!temp.path().join("audit").join("audit.jsonl.1").exists());
}

#[test]
fn real_store_migrates_legacy_bytes_once_and_labels_them_untrusted() {
    let temp = TempDir::new().unwrap();
    let audit_dir = temp.path().join("audit");
    fs::create_dir_all(&audit_dir).unwrap();
    let older = b"{\"tool\":\"auth\",\"outcome\":\"rejected\"}\n";
    let current = b"{\"tool\":\"run\",\"outcome\":\"accepted\"}\n{\"tool\":\"cancel\",\"outcome\":\"accepted\"}\n";
    fs::write(audit_dir.join("audit.jsonl.1"), older).unwrap();
    fs::write(audit_dir.join("audit.jsonl"), current).unwrap();

    let store = OrchStore::open(temp.path()).unwrap();
    let status = store.audit_status();
    assert_eq!(status.imported_generations, 2);
    assert_eq!(status.recovery.imported_generations, 2);
    assert_eq!(fs::read(audit_dir.join("audit.jsonl.1")).unwrap(), older);
    assert_eq!(fs::read(audit_dir.join("audit.jsonl")).unwrap(), current);
    store
        .append_audit(&audit_entry("native", "native-intent", "private"))
        .unwrap();
    assert_eq!(store.audit_status().global_last_seq, 4);
    store.shutdown().unwrap();

    let reopened = OrchStore::open(temp.path()).unwrap();
    assert_eq!(reopened.audit_status().imported_generations, 2);
    assert_eq!(reopened.audit_status().recovery.imported_generations, 0);
    assert_eq!(reopened.audit_status().global_last_seq, 4);
    assert!(reopened
        .export_audit(&temp.path().join("v1"), ExportFormat::V1)
        .is_err());
}

#[test]
fn real_store_tamper_fails_closed_before_use() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store
        .append_audit(&audit_entry("tamper-target", "tamper-intent", "private"))
        .unwrap();
    let generation = store.audit_status().active_generation_id;
    store.shutdown().unwrap();

    let journal = temp
        .path()
        .join("audit")
        .join("generations")
        .join(generation)
        .join("journal.jsonl");
    let mut bytes = fs::read(&journal).unwrap();
    let position = bytes
        .iter()
        .position(|byte| *byte == b'a')
        .expect("journal contains a mutable operation byte");
    bytes[position] = b'b';
    fs::write(journal, bytes).unwrap();
    assert!(OrchStore::open(temp.path()).is_err());
}

#[test]
fn real_store_retention_requires_a_verified_export_and_keeps_tombstone() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store
        .append_audit(&audit_entry(
            "before-rotation",
            "rotation-intent",
            "private",
        ))
        .unwrap();
    store.rotate_audit().unwrap();
    let destination = temp.path().join("retention-export");
    let receipt = store.export_audit(&destination, ExportFormat::V2).unwrap();

    assert!(store
        .retain_audit(RetentionRequest::new("g-000001").with_export_seal("bogus"))
        .is_err());
    let retained = store
        .retain_audit(RetentionRequest::new("g-000001").with_export_seal(receipt.seal_id))
        .unwrap();
    assert_eq!(retained.generation_id, "g-000001");
    assert_eq!(store.audit_status().tombstones, 1);
    assert!(!temp
        .path()
        .join("audit")
        .join("generations")
        .join("g-000001")
        .exists());
    store.shutdown().unwrap();

    let reopened = OrchStore::open(temp.path()).unwrap();
    assert_eq!(reopened.audit_status().tombstones, 1);
    assert!(reopened.verify_audit().unwrap() >= 1);
}

#[test]
fn real_store_concurrent_append_preserves_exact_sequence() {
    let temp = TempDir::new().unwrap();
    let store = Arc::new(OrchStore::open(temp.path()).unwrap());
    let mut workers = Vec::new();
    for worker in 0..4 {
        let store = Arc::clone(&store);
        workers.push(thread::spawn(move || {
            for index in 0..25 {
                store
                    .append_audit(&audit_entry(
                        "concurrent",
                        &format!("worker-{worker}-{index}"),
                        "private",
                    ))
                    .unwrap();
            }
        }));
    }
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(store.audit_status().global_last_seq, 100);
    assert_eq!(store.verify_audit().unwrap(), 1);
}

#[test]
fn two_process_immediate_same_home_reuse_after_explicit_shutdown() {
    if let Ok(path) = std::env::var("GROKPTAH_AUDIT_CHILD_HOME") {
        let store = OrchStore::open(&path).unwrap();
        store
            .append_audit(&audit_entry("child", "child-intent", "private"))
            .unwrap();
        return;
    }

    let temp = TempDir::new().unwrap();
    let root = PathBuf::from(temp.path());
    let store = OrchStore::open(&root).unwrap();
    store
        .append_audit(&audit_entry("parent", "parent-intent", "private"))
        .unwrap();
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
    assert_eq!(reopened.audit_status().global_last_seq, 2);
}

#[test]
fn key_custody_modes_fail_closed_without_safe_material() {
    let temp = TempDir::new().unwrap();
    let key_path = temp.path().join(".audit-key");
    fs::write(&key_path, [0u8; 64]).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    assert!(AuditKeyCustody::headless_service(temp.path()).is_err());
    assert!(AuditKeyCustody::packaged_desktop(temp.path()).is_err());

    let external = AuditKeyCustody::external_consumer(Arc::new(AuditKeys::derive(
        b"external-consumer-held-key",
    )));
    let store = OrchStore::open_with_custody(temp.path().join("external"), external).unwrap();
    assert_eq!(store.audit_status().key_epoch, 1);
    store.shutdown().unwrap();
}

#[test]
fn missing_retired_epoch_fails_closed_instead_of_claiming_clean_audit() {
    let temp = TempDir::new().unwrap();
    let store = OrchStore::open(temp.path()).unwrap();
    store.rotate_audit_key().unwrap();
    store.shutdown().unwrap();
    fs::remove_file(
        temp.path()
            .join(".audit-key-epochs")
            .join("epoch-00000002.key"),
    )
    .unwrap();
    assert!(OrchStore::open(temp.path()).is_err());
}
