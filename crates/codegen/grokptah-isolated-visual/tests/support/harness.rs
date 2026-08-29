// Shared harness for the adversarial matrix. Included with `include!` rather
// than linked as a module so it lives beside the integration test without
// becoming its own test target.

use grokptah_isolated_visual::{
    ids::{sha256_hex, ISOLATED_VISUAL_BACKEND_ID, SCHEMA_VERSION},
    manifest::{IsolatedSourceEntry, SourceObject, SourceObjectKind},
    protocol::{IsolatedInputEvent, IsolatedInputKind},
    ContentAddressedStore, CreateGuestRequest, HelperIdentity, HermeticResolver, IsolatedPreflight,
    IsolatedSourceManifest, IsolatedVisualHost, IsolatedVisualResourceLimits, TestClock,
};
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

pub struct Harness {
    pub _dir: Option<TempDir>,
    pub clock: Arc<TestClock>,
    pub host: IsolatedVisualHost,
}

impl Harness {
    /// Open a host rooted at `root`. Calling this again after the previous
    /// `Harness` is dropped is a real restart against the same durable store.
    pub fn at(root: &Path, clock: &Arc<TestClock>) -> Self {
        let host = IsolatedVisualHost::open_with_preflight(
            root,
            clock.clone(),
            HermeticResolver::new(ContentAddressedStore::new()),
            // No trust root, no artifacts: the honest state on a CI host.
            IsolatedPreflight::denied("test harness: no packaged artifacts"),
        )
        .expect("host opens");
        Self {
            _dir: None,
            clock: clock.clone(),
            host,
        }
    }
}

pub fn harness() -> Harness {
    let dir = TempDir::new().expect("tempdir");
    let clock = Arc::new(TestClock::new(chrono::Utc::now()));
    let mut harness = Harness::at(&dir.path().join("store"), &clock);
    harness._dir = Some(dir);
    harness
}

pub fn source_manifest(store: &mut ContentAddressedStore) -> IsolatedSourceManifest {
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

pub fn request(attempt: &str) -> CreateGuestRequest {
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

pub fn running_guest(harness: &mut Harness, attempt: &str) -> String {
    let guest = harness.host.create_guest(request(attempt)).expect("guest");
    harness.clock.jump(chrono::Duration::seconds(1));
    harness.host.mark_ready(&guest.guest_id).expect("ready");
    harness.clock.jump(chrono::Duration::seconds(1));
    harness.host.mark_running(&guest.guest_id).expect("running");
    guest.guest_id
}

pub fn granted_lease(harness: &mut Harness, guest_id: &str) -> String {
    let guest = harness.host.guest(guest_id).expect("guest");
    let lease = harness.host.enqueue_lease(guest_id).expect("enqueue");
    harness
        .host
        .grant_next(&guest.conflict_domain_id)
        .expect("grant");
    lease.lease_id
}

pub fn dispatch_event(
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
