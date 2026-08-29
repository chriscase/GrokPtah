use std::sync::Arc;

use chrono::{Duration, TimeZone, Utc};
use serde_json::json;
use tempfile::tempdir;

use crate::cleanup::{IsolatedCleanupEvidence, IsolatedCleanupReason};
use crate::clock::TestClock;
use crate::error::IsolatedErrorCode;
use crate::host::{CreateGuestRequest, IsolatedVisualHost};
use crate::ids::{sha256_hex, ISOLATED_VISUAL_BACKEND_ID};
use crate::lease::{ComputerDispatchState, ComputerSurfaceLeaseState};
use crate::lifecycle::{IsolatedEvidenceClass, IsolatedGuestPhase, IsolatedGuestTerminal};
use crate::manifest::{
    HelperIdentity, IsolatedSourceEntry, IsolatedSourceManifest, IsolatedVisualResourceLimits,
    SourceObject, SourceObjectKind,
};
use crate::packaged_authority::{write_guest_image_claim, write_planted_codesign_display};
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
                content_sha256: "d".repeat(64),
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
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    assert!(host.simulator().resident_bytes(&guest.guest_id) > 0);
    host.terminate(&guest.guest_id, IsolatedCleanupReason::Cancel)
        .unwrap();
    let cleaned = host.cleanup(&guest.guest_id).unwrap();
    assert!(cleaned.cleaned);
    assert_eq!(cleaned.resident_frame_bytes, 0);
    assert_eq!(host.simulator().resident_bytes(&guest.guest_id), 0);
}

#[test]
fn helper_failure_cleanup_fails_closed_and_does_not_resume() {
    let (_dir, mut host, _clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    host.terminate(&guest.guest_id, IsolatedCleanupReason::HelperFailure)
        .unwrap();
    let failed = host.guest(&guest.guest_id).unwrap();
    assert_eq!(failed.phase, IsolatedGuestPhase::Closing);
    assert_eq!(failed.terminal, Some(IsolatedGuestTerminal::Failed));
    let cleaned = host.cleanup(&guest.guest_id).unwrap();
    assert!(cleaned.cleaned);
    assert_eq!(cleaned.resident_frame_bytes, 0);
    assert_eq!(
        host.enqueue_lease(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
    assert_eq!(
        host.mark_running(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
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
    assert!(!host.preflight().virtualization_framework_launched_claim());
    assert_eq!(
        host.preflight().evidence_class,
        IsolatedEvidenceClass::SimulatorIneligible
    );
}

#[test]
fn injector_failure_after_injected_is_uncertain_and_not_replayed() {
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
    host.fail_next_injector();
    assert_eq!(
        host.inject_dispatch(&guest.guest_id, &lease.lease_id, event.clone(), false)
            .unwrap_err()
            .code,
        IsolatedErrorCode::UncertainOutcome
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
    assert_eq!(
        host.inject_dispatch(&guest.guest_id, &lease.lease_id, event, false)
            .unwrap_err()
            .code,
        IsolatedErrorCode::UncertainOutcome
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
}

#[test]
fn prepare_dispatch_rejects_same_id_changed_payload() {
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
    host.prepare_dispatch(&guest.guest_id, &lease.lease_id, event.clone())
        .unwrap();
    event.kind = IsolatedInputKind::PointerMove { x: 12, y: 12 };
    assert_eq!(
        host.inject_dispatch(&guest.guest_id, &lease.lease_id, event, false)
            .unwrap_err()
            .code,
        IsolatedErrorCode::Conflict
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
}

#[test]
fn old_incarnation_cannot_accept_input() {
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
    event.incarnation = "old-incarnation".into();
    assert_eq!(
        host.prepare_dispatch(&guest.guest_id, &lease.lease_id, event)
            .unwrap_err()
            .code,
        IsolatedErrorCode::Unauthorized
    );
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
}

#[test]
fn restart_after_prepared_is_known_not_injected() {
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
    host.prepare_dispatch(&guest.guest_id, &lease.lease_id, event)
        .unwrap();
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
    let root = dir.path().to_path_buf();
    drop(host);
    let mut store = ContentAddressedStore::new();
    let _ = source(&mut store);
    let mut host = IsolatedVisualHost::open(root, clock, HermeticResolver::new(store)).unwrap();
    let recovered_lease = host
        .leases()
        .unwrap()
        .into_iter()
        .find(|item| item.lease_id == lease.lease_id)
        .unwrap();
    assert_eq!(
        recovered_lease.dispatch.as_ref().unwrap().state,
        ComputerDispatchState::KnownNotInjected
    );
    assert_eq!(recovered_lease.state, ComputerSurfaceLeaseState::Revoked);
    assert_eq!(host.simulator().input_len(&guest.guest_id), 0);
    assert_eq!(
        host.enqueue_lease(&guest.guest_id).unwrap_err().code,
        IsolatedErrorCode::InvalidState
    );
}

#[test]
fn leftover_overlay_and_occupancy_after_reopen_cannot_mark_cleaned() {
    let (dir, mut host, clock) = open_host();
    let (guest, lease) = create_running(&mut host);
    host.ingest_frame(&guest.guest_id, &lease.lease_id, 2, 2, &[1, 2, 3, 4])
        .unwrap();
    host.terminate(&guest.guest_id, IsolatedCleanupReason::Cancel)
        .unwrap();
    let before = host.observe_cleanup(&guest.guest_id).unwrap();
    assert!(before.overlay_removed.unwrap().overlay_present);
    assert!(before.occupancy_released.unwrap().occupancy_held);
    let root = dir.path().to_path_buf();
    let guest_id = guest.guest_id.clone();
    drop(host);
    let mut store = ContentAddressedStore::new();
    let _ = source(&mut store);
    let host = IsolatedVisualHost::open(root, clock, HermeticResolver::new(store)).unwrap();
    let observation = host.observe_cleanup(&guest_id).unwrap();
    assert!(
        observation
            .overlay_removed
            .as_ref()
            .unwrap()
            .overlay_present
    );
    assert!(
        observation
            .occupancy_released
            .as_ref()
            .unwrap()
            .occupancy_held
    );
    assert!(observation.helper_exit.as_ref().unwrap().helper_alive);
    assert_eq!(
        IsolatedCleanupEvidence::from_observations(observation, Utc::now())
            .unwrap_err()
            .code,
        IsolatedErrorCode::UncertainOutcome
    );
    assert!(!host.guest(&guest_id).unwrap().cleaned);
}

#[test]
fn same_image_occupancy_is_exclusive() {
    let (_dir, mut host, _clock) = open_host();
    let (_guest, _lease) = create_running(&mut host);
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    *host.resolver_mut() = HermeticResolver::new(store);
    assert_eq!(
        host.create_guest(CreateGuestRequest {
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
        .unwrap_err()
        .code,
        IsolatedErrorCode::Conflict
    );
}

#[test]
fn create_packaged_guest_requires_admitted_identity() {
    let (_dir, mut host, _clock) = open_host();
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    *host.resolver_mut() = HermeticResolver::new(store);
    assert!(host
        .create_packaged_guest(CreateGuestRequest {
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
        .is_err());
}

#[test]
fn host_open_uses_canonical_env_pins_for_guest_image() {
    let dir = tempdir().unwrap();
    let observation = write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
    let clock = clock();
    let store = ContentAddressedStore::new();
    let unpinned = IsolatedVisualHost::open_with_artifacts(
        dir.path().join("host-unpinned"),
        clock.clone(),
        HermeticResolver::new(store.clone()),
        Some(dir.path()),
    )
    .unwrap();
    assert!(!unpinned.preflight().image_admitted);
    assert!(unpinned
        .preflight()
        .deny_reason
        .as_deref()
        .unwrap_or("")
        .contains("canonical guest-image identity is not pinned"));
    drop(unpinned);

    let digest_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_DIGEST_ENV;
    let provenance_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_PROVENANCE_ENV;
    let auth_key = crate::packaged_authority::ISOLATED_GUEST_IMAGE_AUTHORIZATION_ENV;
    let previous = [
        (
            digest_key,
            std::env::var(digest_key).ok(),
            observation.digest.clone(),
        ),
        (
            provenance_key,
            std::env::var(provenance_key).ok(),
            observation.provenance.clone(),
        ),
        (
            auth_key,
            std::env::var(auth_key).ok(),
            observation.authorization_digest.clone(),
        ),
    ];
    for (key, _, value) in &previous {
        std::env::set_var(key, value);
    }
    let pinned = IsolatedVisualHost::open_with_artifacts(
        dir.path().join("host-pinned"),
        clock,
        HermeticResolver::new(store),
        Some(dir.path()),
    )
    .unwrap();
    for (key, previous, _) in previous {
        match previous {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    assert!(pinned.preflight().image_admitted);
    assert!(!pinned.preflight().helper_admitted);
    assert!(pinned.preflight().fail_closed_launch().is_err());
    assert_eq!(
        pinned.preflight().evidence_class,
        IsolatedEvidenceClass::SimulatorIneligible
    );
}

#[test]
fn planted_codesign_display_cannot_create_packaged_guest() {
    let dir = tempdir().unwrap();
    write_planted_codesign_display(dir.path(), "TEAMID1234").unwrap();
    write_guest_image_claim(dir.path(), b"guest-bytes").unwrap();
    let clock = clock();
    let mut store = ContentAddressedStore::new();
    let manifest = source(&mut store);
    let mut host = IsolatedVisualHost::open_with_artifacts(
        dir.path().join("host"),
        clock,
        HermeticResolver::new(store),
        Some(dir.path()),
    )
    .unwrap();
    assert!(!host.preflight().helper_admitted);
    assert!(host.preflight().fail_closed_launch().is_err());
    assert!(host
        .create_packaged_guest(CreateGuestRequest {
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
        .is_err());
}

#[test]
fn two_isolated_domains_use_distinct_image_occupancy() {
    let (_dir, mut host, _clock) = open_host();
    let (a, _) = create_running(&mut host);
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
                content_sha256: "d".repeat(64),
                signing_requirement_sha256: "b".repeat(64),
            },
            source: manifest,
            limits: IsolatedVisualResourceLimits::proof_defaults(),
        })
        .unwrap();
    assert_ne!(a.occupancy_resource_key, b.occupancy_resource_key);
}
