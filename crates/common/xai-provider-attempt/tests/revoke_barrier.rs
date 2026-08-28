use std::sync::{Arc, Mutex};

use tempfile::tempdir;
use xai_provider_attempt::{
    AttemptContext, AttemptError, CanonicalHostAuthority, DeterministicFakeTransport,
    ProviderAttemptStore, SendState,
};

fn authority(generation: u64) -> CanonicalHostAuthority {
    CanonicalHostAuthority::from_trusted_host_adapter(
        "live-principal",
        generation,
        generation,
        format!("live-lease-{generation}"),
        format!("live-scope-{generation}"),
    )
    .unwrap()
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

#[test]
fn cloned_one_use_effect_lease_cannot_start_a_second_send() {
    let temp = tempdir().unwrap();
    let store = ProviderAttemptStore::open(temp.path()).unwrap();
    let current = Arc::new(Mutex::new(authority(1)));
    let observed_a = Arc::clone(&current);
    let observed_b = Arc::clone(&current);
    let context_a = AttemptContext::from_host_authority(
        store.clone(),
        "lease-operation-a",
        authority(1),
        Arc::new(move || Some(observed_a.lock().unwrap().clone())),
    )
    .unwrap();
    let context_b = AttemptContext::from_host_authority(
        store,
        "lease-operation-b",
        authority(1),
        Arc::new(move || Some(observed_b.lock().unwrap().clone())),
    )
    .unwrap();

    let transport = DeterministicFakeTransport::default();
    let first = context_a.begin("xai", b"first", true).unwrap();
    let second = context_b.prepare("xai", b"replayed", true).unwrap();
    assert_eq!(
        context_b.begin_send(&second).unwrap_err(),
        AttemptError::EffectLeaseAlreadyUsed
    );
    assert_eq!(first.attempt_id().len(), 36);
    assert!(transport.request_ids().is_empty());
}
