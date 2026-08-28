use std::sync::{Arc, Mutex};

use tempfile::tempdir;
use xai_provider_attempt::{
    AttemptContext, AttemptError, CanonicalHostAuthority, DeterministicFakeTransport,
    ProviderAttemptStore, SendState,
};

fn authority(generation: u64) -> CanonicalHostAuthority {
    CanonicalHostAuthority {
        principal_incarnation: "live-principal".into(),
        principal_generation: generation,
        capability_generation: generation,
        effect_lease: format!("live-lease-{generation}"),
    }
}

#[test]
fn revoke_between_admission_and_begin_send_is_zero_write() {
    let temp = tempdir().unwrap();
    let store = ProviderAttemptStore::open(temp.path()).unwrap();
    let store_for_projection = store.clone();
    let current = Arc::new(Mutex::new(authority(1)));
    let observed = Arc::clone(&current);
    let context = AttemptContext::from_host_authority(
        store,
        "live-operation",
        authority(1),
        Arc::new(move || Some(observed.lock().unwrap().clone())),
    )
    .unwrap();

    let attempt = context.prepare("xai", b"request", true).unwrap();
    let transport = DeterministicFakeTransport::default();
    *current.lock().unwrap() = authority(2);
    assert_eq!(
        context.begin_send(&attempt).unwrap_err(),
        AttemptError::StaleAuthority
    );
    assert_eq!(
        store_for_projection
            .projection(attempt.attempt_id())
            .unwrap()
            .unwrap()
            .send_state,
        SendState::Cancelled
    );
    assert!(transport.request_ids().is_empty());
}
