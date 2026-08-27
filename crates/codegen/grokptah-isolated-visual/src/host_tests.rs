use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use serde_json::json;
use tempfile::tempdir;

use crate::cleanup::{IsolatedCleanupEvidence, IsolatedCleanupReason};
use crate::clock::{HostClock, TestClock};
use crate::error::IsolatedErrorCode;
use crate::host::{CreateGuestRequest, IsolatedVisualHost};
use crate::ids::{sha256_hex, ISOLATED_VISUAL_BACKEND_ID};
use crate::lease::{ComputerDispatchState, ComputerSurfaceLeaseState};
use crate::lifecycle::{IsolatedEvidenceClass, IsolatedGuestPhase, IsolatedGuestTerminal};
use crate::manifest::{
    HelperIdentity, IsolatedSourceEntry, IsolatedSourceManifest, IsolatedVisualResourceLimits,
    SourceObject, SourceObjectKind,
};
use crate::projection::redact_public_value;
use crate::protocol::{IsolatedInputEvent, IsolatedInputKind};
use crate::resolver::{ContentAddressedStore, HermeticResolver};

fn clock() -> Arc<TestClock> {
    Arc::new(TestClock::new(
        Utc.with_ymd_and_hms(2026, 8, 27, 12, 0, 0).unwrap(),
    ))
}

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

fn open_host() -> (tempfile::TempDir, IsolatedVisualHost, Arc<TestClock>) {
    let dir = tempdir().unwrap();
    let clock = clock();
    let mut store = ContentAddressedStore::new();
    let _ = source(&mut store);
    let host =
        IsolatedVisualHost::open(dir.path(), clock.clone(), HermeticResolver::new(store)).unwrap();
    (dir, host, clock)
}

fn create_running(
    host: &mut IsolatedVisualHost,
) -> (
    crate::lifecycle::IsolatedGuestRecord,
    crate::lease::ComputerSurfaceLease,
) {
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    *host.resolver_mut() = HermeticResolver::new(store);
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
    host.mark_running(&guest.guest_id).unwrap();
    let queued = host.enqueue_lease(&guest.guest_id).unwrap();
    let granted = host.grant_next(&queued.conflict_domain_id).unwrap();
    (host.guest(&guest.guest_id).unwrap(), granted)
}

fn pointer(
    guest: &crate::lifecycle::IsolatedGuestRecord,
    lease: &crate::lease::ComputerSurfaceLease,
) -> IsolatedInputEvent {
    IsolatedInputEvent {
        dispatch_id: "dispatch-1".into(),
        guest_id: guest.guest_id.clone(),
        lease_id: lease.lease_id.clone(),
        lease_revision: lease.revision,
        surface_id: guest.surface.surface_id.clone(),
        incarnation: guest.surface.incarnation.clone(),
        frame_epoch: guest.frame_epoch,
        kind: IsolatedInputKind::PointerMove { x: 10, y: 10 },
    }
}

#[test]
fn lifecycle_create_ready_running_closing_and_no_resume() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    assert_eq!(guest.phase, IsolatedGuestPhase::Running);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    host.terminate(&guest.guest_id, IsolatedCleanupReason::Cancel)
        .unwrap();
    let closed = host.guest(&guest.guest_id).unwrap();
    assert_eq!(closed.phase, IsolatedGuestPhase::Closing);
    assert_eq!(closed.terminal, Some(IsolatedGuestTerminal::Interrupted));
    assert_eq!(
        host.enqueue_lease(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
}

#[test]
fn one_agent_per_guest_and_forged_identities_denied() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    assert_eq!(
        host.enqueue_lease(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::Conflict
    );
    let mut event = pointer(&guest, &lease);
    event.guest_id = "forged-guest".into();
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    event.frame_epoch = 1;
    event.lease_revision = lease.revision;
    assert_eq!(
        host.prepare_dispatch(&guest.guest_id, &lease.lease_id, event)
            .unwrap_err()
            .code,
        IsolatedErrorCode::Unauthorized
    );
}

#[test]
fn two_isolated_domains_dispatch_in_parallel() {
    let (_dir, mut host, _clock) = open_host();
    let (a, a_lease) = create_running(&mut host);
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    *host.resolver_mut() = HermeticResolver::new(store);
    let b = host
        .create_guest(CreateGuestRequest {
            run_id: "run-2".into(),
            work_id: "work-2".into(),
            work_attempt_id: "attempt-2".into(),
            agent_id: "agent-2".into(),
            agent_spec_revision: 1,
            helper: HelperIdentity {
                helper_id: "helper-2".into(),
                content_sha256: "a".repeat(64),
                signing_requirement_sha256: "b".repeat(64),
            },
            source: manifest,
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        })
        .unwrap();
    host.mark_ready(&b.guest_id).unwrap();
    host.mark_running(&b.guest_id).unwrap();
    let b_queued = host.enqueue_lease(&b.guest_id).unwrap();
    let b_lease = host.grant_next(&b_queued.conflict_domain_id).unwrap();
    assert_ne!(a.conflict_domain_id, b.conflict_domain_id);
    host.ingest_frame(&a.guest_id, &a_lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    host.ingest_frame(&b.guest_id, &b_lease.lease_id, 2, 2, &[5, 6, 7, 8])
        .unwrap();
    let a_guest = host.guest(&a.guest_id).unwrap();
    let b_guest = host.guest(&b.guest_id).unwrap();
    let mut a_event = pointer(&a_guest, &a_lease);
    a_event.frame_epoch = a_guest.frame_epoch;
    a_event.lease_revision = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|lease| lease.lease_id == a_lease.lease_id)
        .unwrap()
        .revision;
    let mut b_event = pointer(&b_guest, &b_lease);
    b_event.dispatch_id = "dispatch-2".into();
    b_event.frame_epoch = b_guest.frame_epoch;
    b_event.lease_revision = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|lease| lease.lease_id == b_lease.lease_id)
        .unwrap()
        .revision;
    host.inject_dispatch(&a.guest_id, &a_lease.lease_id, a_event, false)
        .unwrap();
    host.inject_dispatch(&b.guest_id, &b_lease.lease_id, b_event, false)
        .unwrap();
    assert_eq!(host.simulator().input_len(&a.guest_id), 1);
    assert_eq!(host.simulator().input_len(&b.guest_id), 1);
}

#[test]
fn same_domain_serializes_and_stale_frame_is_zero_backend_effect() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    assert!(host.grant_next(&lease.conflict_domain_id).is_err());
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[9, 9, 9, 9])
        .unwrap();
    let guest = host.guest(&guest.guest_id).unwrap();
    let lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    let mut stale = pointer(&guest, &lease);
    stale.frame_epoch = guest.frame_epoch - 1;
    stale.lease_revision = lease.revision;
    assert_eq!(
        host.prepare_dispatch(&guest.guest_id, &lease.lease_id, stale)
            .unwrap_err()
            .code,
        IsolatedErrorCode::StaleObservation
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
}

#[test]
fn duplicate_dispatch_is_exactly_once() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    let guest = host.guest(&guest.guest_id).unwrap();
    let lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    let mut event = pointer(&guest, &lease);
    event.frame_epoch = guest.frame_epoch;
    event.lease_revision = lease.revision;
    let first = host
        .inject_dispatch(&guest.guest_id, &lease.lease_id, event.clone(), false)
        .unwrap();
    assert_eq!(
        first.dispatch.as_ref().unwrap().state,
        ComputerDispatchState::Acknowledged
    );
    let second = host
        .inject_dispatch(&guest.guest_id, &lease.lease_id, event, false)
        .unwrap();
    assert_eq!(
        second.dispatch.as_ref().unwrap().dispatch_id,
        first.dispatch.as_ref().unwrap().dispatch_id
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 1);
}

#[test]
fn dispatching_lease_rejects_a_second_dispatch_id() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    let guest = host.guest(&guest.guest_id).unwrap();
    let lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    let mut first = pointer(&guest, &lease);
    first.frame_epoch = guest.frame_epoch;
    first.lease_revision = lease.revision;
    host.prepare_dispatch(&guest.guest_id, &lease.lease_id, first.clone())
        .unwrap();
    let mut second = first.clone();
    second.dispatch_id = "dispatch-other".into();
    second.kind = IsolatedInputKind::PointerMove { x: 11, y: 11 };
    assert_eq!(
        host.inject_dispatch(&guest.guest_id, &lease.lease_id, second, false)
            .unwrap_err()
            .code,
        IsolatedErrorCode::Conflict
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
}

#[test]
fn crash_after_inject_then_two_restarts_do_not_replay() {
    let (dir, mut host, clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    let guest = host.guest(&guest.guest_id).unwrap();
    let lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    let mut event = pointer(&guest, &lease);
    event.frame_epoch = guest.frame_epoch;
    event.lease_revision = lease.revision;
    host.inject_dispatch(&guest.guest_id, &lease.lease_id, event, true)
        .unwrap();
    let root = dir.path().to_path_buf();
    drop(host);
    let mut store = ContentAddressedStore::new();
    let _ = source(&mut store);
    let host = IsolatedVisualHost::open(
        root.clone(),
        clock.clone(),
        HermeticResolver::new(store.clone()),
    )
    .unwrap();
    let recovered = host.guest(&guest.guest_id).unwrap();
    assert_eq!(recovered.terminal, Some(IsolatedGuestTerminal::Interrupted));
    assert_ne!(recovered.surface.incarnation, guest.surface.incarnation);
    let recovered_lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    assert_eq!(recovered_lease.state, ComputerSurfaceLeaseState::Uncertain);
    drop(host);
    let mut host = IsolatedVisualHost::open(root, clock, HermeticResolver::new(store)).unwrap();
    let second = host.guest(&guest.guest_id).unwrap();
    assert_eq!(second.guest_id, recovered.guest_id);
    assert_eq!(second.surface.incarnation, recovered.surface.incarnation);
    assert_eq!(
        host.enqueue_lease(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
}

#[test]
fn public_projection_redacts_secrets_and_omits_frame_bytes() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    let projection = host.project(&guest.guest_id).unwrap();
    let encoded = serde_json::to_value(&projection).unwrap();
    assert!(encoded.get("bytes").is_none());
    redact_public_value(&encoded).unwrap();
    assert_eq!(
        projection.evidence_class,
        IsolatedEvidenceClass::SimulatorIneligible
    );
    assert!(!projection.virtualization_framework_launched);
    assert!(redact_public_value(&json!({ "note": "/Users/chris/.ssh/id_rsa" })).is_err());
}

#[test]
fn cleanup_after_cancel_drops_residency() {
    let (_dir, mut host, clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    assert!(host.simulator().resident_bytes(&guest.guest_id) > 0);
    host.terminate(&guest.guest_id, IsolatedCleanupReason::Cancel)
        .unwrap();
    let guest = host.guest(&guest.guest_id).unwrap();
    let evidence =
        IsolatedCleanupEvidence::verified(&guest.guest_id, guest.surface.clone(), clock.now())
            .unwrap();
    let cleaned = host.cleanup(&guest.guest_id, evidence).unwrap();
    assert!(cleaned.cleaned);
    assert_eq!(cleaned.resident_frame_bytes, 0);
    assert_eq!(host.simulator().resident_bytes(&guest.guest_id), 0);
}

#[test]
fn resolver_object_substitution_and_rename_fail_closed() {
    let dir = tempdir().unwrap();
    let mut store = ContentAddressedStore::new();
    let good = store.insert(b"int main(void) { return 0; }\n");
    let evil = store.insert(b"int pwn(void) { return 1; }\n");
    let mut manifest = IsolatedSourceManifest {
        schema_version: 1,
        backend_id: ISOLATED_VISUAL_BACKEND_ID.into(),
        guest_protocol_version: 1,
        objects: vec![IsolatedSourceEntry {
            relative_path: "guest-init.c".into(),
            object: SourceObject {
                digest_sha256: good.clone(),
                kind: SourceObjectKind::Blob,
                media_type: "text/x-c".into(),
                byte_len: 29,
            },
        }],
        helper_content_sha256: "a".repeat(64),
        helper_signing_requirement_sha256: "b".repeat(64),
        guest_image_sha256: None,
        configuration_sha256: "c".repeat(64),
    };
    let resolver = HermeticResolver::new(store);
    resolver.resolve(&manifest, &dir.path().join("ok")).unwrap();
    manifest.objects[0].object.digest_sha256 = evil;
    assert!(resolver
        .resolve(&manifest, &dir.path().join("evil"))
        .is_err());
    assert_eq!(sha256_hex(b"int main(void) { return 0; }\n"), good);
}

#[test]
fn lease_expiry_uses_host_clock() {
    let (_dir, mut host, clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    clock.jump(Duration::minutes(6));
    let guest = host.guest(&guest.guest_id).unwrap();
    let lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    let mut event = pointer(&guest, &lease);
    event.frame_epoch = guest.frame_epoch;
    event.lease_revision = lease.revision;
    assert_eq!(
        host.prepare_dispatch(&guest.guest_id, &lease.lease_id, event)
            .unwrap_err()
            .code,
        IsolatedErrorCode::Unauthorized
    );
}

#[test]
fn preflight_is_ineligible_for_vm_qualification() {
    let (_dir, host, _clock) = open_host();
    assert!(!host.preflight().allowed_to_launch);
    assert!(host.preflight().fail_closed_launch().is_err());
}
