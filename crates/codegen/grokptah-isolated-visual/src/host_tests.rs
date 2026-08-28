//! End-to-end tests for the single host authority.
//!
//! These use the deterministic simulator and a test clock. They never launch a
//! VM, request macOS permissions, or dispatch OS input, and the evidence class
//! stays `SimulatorIneligible` throughout — which the tests assert.

use std::sync::Arc;

use chrono::{Duration, Utc};
use tempfile::TempDir;

use crate::cleanup::{CleanupOutcome, IsolatedCleanupReason};
use crate::clock::TestClock;
use crate::error::IsolatedErrorCode;
use crate::host::{CreateGuestRequest, IsolatedVisualHost};
use crate::ids::{sha256_hex, ISOLATED_VISUAL_BACKEND_ID, SCHEMA_VERSION};
use crate::lease::{ComputerDispatchState, ComputerSurfaceLeaseState};
use crate::lifecycle::IsolatedEvidenceClass;
use crate::manifest::{
    HelperIdentity, IsolatedSourceEntry, IsolatedSourceManifest, IsolatedVisualResourceLimits,
    SourceObject, SourceObjectKind,
};
use crate::preflight::IsolatedPreflight;
use crate::protocol::{IsolatedInputEvent, IsolatedInputKind};
use crate::resolver::{ContentAddressedStore, HermeticResolver};

pub(crate) struct Harness {
    pub dir: TempDir,
    pub clock: Arc<TestClock>,
    pub host: IsolatedVisualHost,
}

/// Open a host over `dir`. Calling this again after dropping the previous host
/// is a real process restart as far as the durable store is concerned.
pub(crate) fn open_host(dir: &TempDir, clock: &Arc<TestClock>) -> IsolatedVisualHost {
    IsolatedVisualHost::open_with_preflight(
        dir.path().join("store"),
        clock.clone(),
        HermeticResolver::new(ContentAddressedStore::new()),
        // No trust root and no artifacts: the honest state on a test host.
        IsolatedPreflight::inspect(
            None,
            None,
            &crate::code_identity::SystemCodeIdentityProbe,
            None,
        ),
    )
    .expect("host opens")
}

pub(crate) fn source_manifest(store: &mut ContentAddressedStore) -> IsolatedSourceManifest {
    let body = b"int main(void) { return 0; }\n";
    let digest = store.insert(body);
    IsolatedSourceManifest {
        schema_version: SCHEMA_VERSION,
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
        helper_content_sha256: sha256_hex(b"helper"),
        helper_signing_requirement_sha256: sha256_hex(b"requirement"),
        guest_image_sha256: None,
        configuration_sha256: sha256_hex(b"configuration"),
    }
}

pub(crate) fn request(attempt: &str) -> CreateGuestRequest {
    let mut store = ContentAddressedStore::new();
    CreateGuestRequest {
        run_id: format!("run-{attempt}"),
        work_id: format!("work-{attempt}"),
        work_attempt_id: format!("attempt-{attempt}"),
        agent_id: format!("agent-{attempt}"),
        agent_spec_revision: 1,
        helper: HelperIdentity {
            helper_id: format!("helper-{attempt}"),
            content_sha256: sha256_hex(b"helper"),
            signing_requirement_sha256: sha256_hex(b"requirement"),
        },
        source: source_manifest(&mut store),
        limits: IsolatedVisualResourceLimits::proof_defaults(),
    }
}

pub(crate) fn harness() -> Harness {
    let dir = TempDir::new().expect("tempdir");
    let clock = Arc::new(TestClock::new(Utc::now()));
    let host = open_host(&dir, &clock);
    Harness { dir, clock, host }
}

fn running_guest(harness: &mut Harness, attempt: &str) -> String {
    let guest = harness.host.create_guest(request(attempt)).expect("guest");
    harness.clock.jump(Duration::seconds(1));
    harness.host.mark_ready(&guest.guest_id).expect("ready");
    harness.clock.jump(Duration::seconds(1));
    harness.host.mark_running(&guest.guest_id).expect("running");
    guest.guest_id
}

fn granted_lease(harness: &mut Harness, guest_id: &str) -> String {
    let guest = harness.host.guest(guest_id).expect("guest");
    let lease = harness.host.enqueue_lease(guest_id).expect("enqueue");
    harness
        .host
        .grant_next(&guest.conflict_domain_id)
        .expect("grant");
    lease.lease_id
}

fn frame(harness: &mut Harness, guest_id: &str, lease_id: &str) -> u64 {
    harness
        .host
        .ingest_frame(guest_id, lease_id, 8, 8, b"frame-bytes")
        .expect("frame")
        .frame_epoch
}

/// Build an input event bound to the guest's live surface, lease revision, and
/// frame epoch, so tests exercise the fences rather than trip over them.
fn dispatch_event(
    harness: &mut Harness,
    guest_id: &str,
    lease_id: &str,
    dispatch_id: &str,
    key: &str,
) -> IsolatedInputEvent {
    let guest = harness.host.guest(guest_id).expect("guest");
    let lease = harness
        .host
        .leases()
        .expect("leases")
        .into_iter()
        .find(|lease| lease.lease_id == lease_id)
        .expect("lease");
    IsolatedInputEvent {
        dispatch_id: dispatch_id.into(),
        guest_id: guest_id.into(),
        lease_id: lease_id.into(),
        lease_revision: lease.revision,
        surface_id: guest.surface.surface_id.clone(),
        incarnation: guest.surface.incarnation.clone(),
        frame_epoch: guest.frame_epoch,
        kind: IsolatedInputKind::Key {
            code: key.into(),
            pressed: true,
        },
    }
}

#[test]
fn lifecycle_create_ready_running_closing_and_no_resume() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    harness.clock.jump(Duration::seconds(1));
    harness
        .host
        .terminate(&guest_id, IsolatedCleanupReason::Success)
        .expect("terminate");
    let guest = harness.host.guest(&guest_id).expect("guest");
    assert_eq!(guest.phase, crate::lifecycle::IsolatedGuestPhase::Closing);
    // A closing guest cannot go back to running.
    assert!(harness.host.mark_running(&guest_id).is_err());
}

#[test]
fn preflight_is_ineligible_for_vm_qualification() {
    let harness = harness();
    let preflight = harness.host.preflight();
    assert!(!preflight.allowed_to_launch);
    assert!(!preflight.virtualization_framework_launched_claim());
    assert_eq!(
        preflight.evidence_class,
        IsolatedEvidenceClass::SimulatorIneligible
    );
    // The guest record agrees: simulator evidence is never VM evidence.
    let mut harness = harness;
    let guest_id = running_guest(&mut harness, "a");
    assert_eq!(
        harness.host.guest(&guest_id).unwrap().evidence_class,
        IsolatedEvidenceClass::SimulatorIneligible
    );
}

#[test]
fn duplicate_dispatch_is_exactly_once() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    let event = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");

    harness
        .host
        .inject_dispatch(&guest_id, &lease_id, event.clone(), false)
        .expect("first inject");
    assert_eq!(harness.host.simulator().input_len(&guest_id), 1);

    // Replaying the identical dispatch does not inject a second time.
    let replay = harness
        .host
        .inject_dispatch(&guest_id, &lease_id, event, false);
    assert!(replay.is_ok(), "identical replay is idempotent");
    assert_eq!(harness.host.simulator().input_len(&guest_id), 1);
}

#[test]
fn duplicate_dispatch_id_with_a_changed_payload_is_a_conflict() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    let first = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");
    harness
        .host
        .inject_dispatch(&guest_id, &lease_id, first.clone(), false)
        .expect("first inject");

    let mut changed = first;
    changed.kind = IsolatedInputKind::Key {
        code: "z".into(),
        pressed: true,
    };
    let error = harness
        .host
        .inject_dispatch(&guest_id, &lease_id, changed, false)
        .unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Conflict);
    assert_eq!(harness.host.simulator().input_len(&guest_id), 1);
}

#[test]
fn crash_after_inject_then_two_restarts_do_not_replay() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    let event = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");

    // Crash cut: Injected is durable, the acknowledgement never happens.
    harness
        .host
        .inject_dispatch(&guest_id, &lease_id, event.clone(), true)
        .expect("injected then crashed");

    let Harness { dir, clock, host } = harness;
    drop(host);
    let mut dir = dir;
    let mut clock = clock;
    for restart in 1..=2 {
        clock.jump(Duration::seconds(1));
        // A genuine restart: the previous host is gone and its store lock with it.
        let mut harness = Harness {
            host: open_host(&dir, &clock),
            dir,
            clock,
        };
        let lease = harness
            .host
            .leases()
            .expect("leases")
            .into_iter()
            .find(|lease| lease.lease_id == lease_id)
            .expect("lease survives restart");
        assert_eq!(
            lease.state,
            ComputerSurfaceLeaseState::Uncertain,
            "restart {restart}: injected dispatch must become Uncertain"
        );
        assert_eq!(
            lease.dispatch.as_ref().unwrap().state,
            ComputerDispatchState::Uncertain
        );
        // The guest incarnation is not resumable, so nothing can replay onto it.
        assert!(!harness.host.guest(&guest_id).unwrap().is_live());
        assert!(harness
            .host
            .inject_dispatch(&guest_id, &lease_id, event.clone(), false)
            .is_err());
        let Harness {
            dir: d,
            clock: c,
            host,
        } = harness;
        drop(host);
        dir = d;
        clock = c;
        let _ = restart;
    }
}

#[test]
fn one_agent_per_guest_and_forged_identities_denied() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);

    // A second lease on the same guest is refused.
    assert!(harness.host.enqueue_lease(&guest_id).is_err());

    // A lease id that belongs to nothing is refused.
    frame(&mut harness, &guest_id, &lease_id);
    let mut forged = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");
    forged.lease_id = "lease-does-not-exist".into();
    let error = harness
        .host
        .prepare_dispatch(&guest_id, "lease-does-not-exist", forged)
        .unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Unauthorized);

    // A forged surface incarnation is refused by the identity fence.
    let mut wrong_surface = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-2", "a");
    wrong_surface.incarnation = "other-incarnation".into();
    let error = harness
        .host
        .prepare_dispatch(&guest_id, &lease_id, wrong_surface)
        .unwrap_err();
    assert_eq!(error.code, IsolatedErrorCode::Unauthorized);
}

#[test]
fn forged_conflict_domain_cannot_steal_capacity() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let guest = harness.host.guest(&guest_id).unwrap();
    harness.host.enqueue_lease(&guest_id).expect("enqueue");
    // The conflict domain is host-derived from the guest id; a caller-claimed
    // one matches no queued lease.
    assert!(harness.host.grant_next("conflict-i-made-up").is_err());
    harness
        .host
        .grant_next(&guest.conflict_domain_id)
        .expect("real domain grants");
}

#[test]
fn an_expired_lease_is_reaped_and_cannot_dispatch() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    let event = dispatch_event(&mut harness, &guest_id, &lease_id, "dispatch-1", "a");

    // Past the 5-minute grant window.
    harness.clock.jump(Duration::minutes(6));
    let error = harness
        .host
        .prepare_dispatch(&guest_id, &lease_id, event)
        .unwrap_err();
    // Reaping revokes the lease and advances its revision, so the denial may
    // arrive from either fence. What matters is that it is a denial.
    assert!(
        matches!(
            error.code,
            IsolatedErrorCode::Unauthorized
                | IsolatedErrorCode::InvalidState
                | IsolatedErrorCode::StaleObservation
        ),
        "expired lease must not dispatch, got {error:?}"
    );
    assert_eq!(harness.host.simulator().input_len(&guest_id), 0);
    let lease = harness
        .host
        .leases()
        .unwrap()
        .into_iter()
        .find(|lease| lease.lease_id == lease_id)
        .unwrap();
    assert!(lease.state.is_terminal(), "expired lease must be reaped");
}

#[test]
fn cleanup_reports_exact_only_when_every_resource_is_gone() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    harness.clock.jump(Duration::seconds(1));
    harness
        .host
        .terminate(&guest_id, IsolatedCleanupReason::Success)
        .expect("terminate");
    let (guest, receipt) = harness.host.cleanup(&guest_id).expect("cleanup");
    assert_eq!(
        receipt.outcome,
        CleanupOutcome::Exact,
        "{:?}",
        receipt.unresolved
    );
    assert!(guest.cleaned);
    receipt.require_exact().expect("exact receipt");
    // Every required resource is individually digested.
    assert_eq!(
        receipt.probe_digests.len(),
        crate::cleanup::REQUIRED_RESOURCES.len()
    );
}

#[test]
fn a_failed_overlay_deletion_is_uncertain_and_the_guest_is_not_cleaned() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    harness.clock.jump(Duration::seconds(1));
    harness
        .host
        .terminate(&guest_id, IsolatedCleanupReason::Success)
        .expect("terminate");

    // Make the deletion genuinely fail: replace the overlay file with a
    // non-empty directory, which `remove_file` refuses on every platform.
    // Nothing is stubbed; the real teardown path hits a real error.
    let overlay = harness
        .host
        .store_root()
        .join("overlays")
        .join(format!("{guest_id}.overlay"));
    assert!(overlay.exists());
    std::fs::remove_file(&overlay).expect("clear file");
    std::fs::create_dir(&overlay).expect("directory in its place");
    std::fs::write(overlay.join("occupant"), b"x").expect("non-empty");

    let (guest, receipt) = harness.host.cleanup(&guest_id).expect("cleanup runs");

    assert_eq!(receipt.outcome, CleanupOutcome::Unresolved);
    assert!(
        receipt.unresolved.iter().any(|r| r.contains("overlay")),
        "{:?}",
        receipt.unresolved
    );
    assert!(!guest.cleaned, "an unresolved receipt must not mark clean");
    assert_eq!(
        receipt.require_exact().unwrap_err().code,
        IsolatedErrorCode::UncertainOutcome
    );
}

#[test]
fn store_lock_rejects_a_second_open() {
    let harness = harness();
    let root = harness.host.store_root().to_path_buf();
    let second = crate::store::IsolatedVisualStore::open(&root, Utc::now());
    assert_eq!(
        second.err().map(|error| error.code),
        Some(IsolatedErrorCode::Conflict)
    );
}

#[test]
fn public_projection_redacts_secrets_and_omits_frame_bytes() {
    let mut harness = harness();
    let guest_id = running_guest(&mut harness, "a");
    let lease_id = granted_lease(&mut harness, &guest_id);
    frame(&mut harness, &guest_id, &lease_id);
    let projection = harness.host.project(&guest_id).expect("projection");
    let json = serde_json::to_value(&projection).expect("json");
    crate::projection::redact_public_value(&json).expect("projection is already clean");
    let text = json.to_string();
    assert!(!text.contains("frame-bytes"));
    assert!(!text.contains("overlay"));
    assert!(!text.contains("channelSecret"));
    assert!(!projection.virtualization_framework_launched);
}

#[test]
fn resolver_rejects_absent_objects_length_lies_and_renames() {
    let mut harness = harness();
    let mut store = ContentAddressedStore::new();
    let manifest = source_manifest(&mut store);
    *harness.host.resolver_mut().store_mut() = store;
    harness
        .host
        .resolve_source(&manifest, &harness.dir.path().join("stage-ok"))
        .expect("clean resolve");

    // A digest that is not in the declared closure cannot be resolved. The
    // store is content-addressed, so this is how object substitution presents:
    // the manifest names bytes nobody committed to.
    let mut absent = manifest.clone();
    absent.objects[0].object.digest_sha256 = sha256_hex(b"bytes that were never inserted");
    assert!(harness
        .host
        .resolve_source(&absent, &harness.dir.path().join("stage-absent"))
        .is_err());

    // A manifest that lies about the object length is refused even though the
    // digest is present and self-consistent.
    let mut wrong_len = manifest.clone();
    wrong_len.objects[0].object.byte_len += 1;
    assert!(harness
        .host
        .resolve_source(&wrong_len, &harness.dir.path().join("stage-len"))
        .is_err());

    // Renaming to a path outside the allowlist is refused before any write.
    let mut renamed = manifest;
    renamed.objects[0].relative_path = "../escape.c".into();
    let staging = harness.dir.path().join("stage-esc");
    assert!(harness.host.resolve_source(&renamed, &staging).is_err());
    assert!(!harness.dir.path().join("escape.c").exists());
}
