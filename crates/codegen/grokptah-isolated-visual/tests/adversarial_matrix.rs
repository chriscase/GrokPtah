use std::fs;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use chrono::{TimeZone, Utc};
use grokptah_isolated_visual::ids::ISOLATED_VISUAL_BACKEND_ID;
use grokptah_isolated_visual::manifest::{
    IsolatedSourceEntry, IsolatedSourceManifest, SourceObject, SourceObjectKind,
};
use grokptah_isolated_visual::resolver::{ContentAddressedStore, HermeticResolver};
use grokptah_isolated_visual::{
    ComputerSurfaceLeaseState, CreateGuestRequest, HelperIdentity, HostClock,
    IsolatedCleanupEvidence, IsolatedCleanupReason, IsolatedErrorCode, IsolatedGuestTerminal,
    IsolatedVisualHost, IsolatedVisualResourceLimits, IsolatedVisualStore, TestClock,
};
use tempfile::tempdir;

fn source(store: &mut ContentAddressedStore) -> IsolatedSourceManifest {
    let body = b"int main(void) { return 0; }\n";
    let digest = store.insert(body);
    IsolatedSourceManifest {
        schema_version: 1,
        backend_id: ISOLATED_VISUAL_BACKEND_ID.into(),
        guest_protocol_version: 1,
        objects: vec![IsolatedSourceEntry {
            relative_path: "guest-init.c".into(),
            object: SourceObject {
                digest_sha256: digest,
                kind: SourceObjectKind::Blob,
                media_type: "text/x-c".into(),
                byte_len: body.len() as u64,
            },
        }],
        helper_content_sha256: "a".repeat(64),
        helper_signing_requirement_sha256: "b".repeat(64),
        guest_image_sha256: None,
        configuration_sha256: "c".repeat(64),
    }
}

#[test]
fn corrupted_and_legacy_records_fail_closed() {
    let dir = tempdir().unwrap();
    let clock = Arc::new(TestClock::new(
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap(),
    ));
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    let mut host = IsolatedVisualHost::open(
        dir.path(),
        clock.clone(),
        HermeticResolver::new(store.clone()),
    )
    .unwrap();
    host.create_guest(CreateGuestRequest {
        run_id: "run-1".into(),
        work_id: "work-1".into(),
        work_attempt_id: "attempt-1".into(),
        agent_id: "agent-1".into(),
        agent_spec_revision: 1,
        helper: HelperIdentity {
            helper_id: "helper-1".into(),
            content_sha256: "a".repeat(64),
            signing_requirement_sha256: "b".repeat(64),
        },
        source: manifest,
        limits: IsolatedVisualResourceLimits::proof_defaults(),
    })
    .unwrap();
    drop(host);
    fs::write(
        dir.path().join("guests").join("legacy.json"),
        "{\"schemaVersion\":0}",
    )
    .unwrap();
    let recovered =
        IsolatedVisualHost::open(dir.path(), clock, HermeticResolver::new(store)).unwrap();
    let guests_dir = dir.path().join("guests");
    assert!(!guests_dir.join("legacy.json").exists());
    assert!(dir.path().join("quarantine").join("legacy.json").exists());
    drop(recovered);
}

#[test]
fn symlink_and_traversal_in_source_tree_are_denied() {
    let dir = tempdir().unwrap();
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    let resolver = HermeticResolver::new(store);
    let stage = dir.path().join("stage");
    resolver.resolve(&manifest, &stage).unwrap();
    symlink("/etc/passwd", stage.join("link-passwd")).unwrap();
    assert!(resolver.resolve(&manifest, &stage).is_err());
    let mut evil = manifest.clone();
    evil.objects[0].relative_path = "../passwd".into();
    assert!(evil.validate().is_err());
}

#[test]
fn cleanup_failure_is_uncertain_and_guest_is_not_cleaned() {
    let dir = tempdir().unwrap();
    let clock = Arc::new(TestClock::new(
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap(),
    ));
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    let mut host =
        IsolatedVisualHost::open(dir.path(), clock.clone(), HermeticResolver::new(store)).unwrap();
    let guest = host
        .create_guest(CreateGuestRequest {
            run_id: "run-1".into(),
            work_id: "work-1".into(),
            work_attempt_id: "attempt-1".into(),
            agent_id: "agent-1".into(),
            agent_spec_revision: 1,
            helper: HelperIdentity {
                helper_id: "helper-1".into(),
                content_sha256: "a".repeat(64),
                signing_requirement_sha256: "b".repeat(64),
            },
            source: manifest,
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        })
        .unwrap();
    host.terminate(&guest.guest_id, IsolatedCleanupReason::GuestCrash)
        .unwrap();
    let mut evidence =
        IsolatedCleanupEvidence::verified(&guest.guest_id, guest.surface.clone(), clock.now())
            .unwrap();
    evidence.overlay_removed = false;
    assert_eq!(
        host.cleanup(&guest.guest_id, evidence).unwrap_err().code,
        IsolatedErrorCode::UncertainOutcome
    );
    assert!(!host.guest(&guest.guest_id).unwrap().cleaned);
    assert_eq!(
        host.guest(&guest.guest_id).unwrap().terminal,
        Some(IsolatedGuestTerminal::Failed)
    );
}

#[test]
fn store_lock_rejects_a_second_open() {
    let dir = tempdir().unwrap();
    let clock = Arc::new(TestClock::new(
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap(),
    ));
    let first = IsolatedVisualStore::open(dir.path(), clock.now()).unwrap();
    assert!(IsolatedVisualStore::open(dir.path(), clock.now()).is_err());
    drop(first);
}

#[test]
fn forged_conflict_domain_cannot_steal_capacity() {
    let dir = tempdir().unwrap();
    let clock = Arc::new(TestClock::new(
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap(),
    ));
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    let mut host =
        IsolatedVisualHost::open(dir.path(), clock, HermeticResolver::new(store)).unwrap();
    let guest = host
        .create_guest(CreateGuestRequest {
            run_id: "run-1".into(),
            work_id: "work-1".into(),
            work_attempt_id: "attempt-1".into(),
            agent_id: "agent-1".into(),
            agent_spec_revision: 1,
            helper: HelperIdentity {
                helper_id: "helper-1".into(),
                content_sha256: "a".repeat(64),
                signing_requirement_sha256: "b".repeat(64),
            },
            source: manifest,
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        })
        .unwrap();
    host.mark_ready(&guest.guest_id).unwrap();
    host.enqueue_lease(&guest.guest_id).unwrap();
    assert_eq!(
        host.grant_next("conflict-forged").unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
    let granted = host.grant_next(&guest.conflict_domain_id).unwrap();
    assert_eq!(granted.state, ComputerSurfaceLeaseState::Granted);
}
