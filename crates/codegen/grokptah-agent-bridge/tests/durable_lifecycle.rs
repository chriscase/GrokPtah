//! Durable lifecycle, lease, restart, and send-cut tests.

use grokptah_agent_bridge::orchestration::{
    unsigned_provider_spec, DurableAdmission, ExecutionLifecycle, LiveRevisions, MacKey,
    ProviderSendState, Revision, SendRecovery, SpineError, SpinePersist,
};
use tempfile::tempdir;

fn key() -> MacKey {
    MacKey::from_bytes(&[0x77; 32]).unwrap()
}

#[test]
fn starting_running_and_send_lattice_persist() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let spec = unsigned_provider_spec("life", "do-work")
        .seal(&key())
        .unwrap();
    let admitted = admission
        .admit(&key(), spec, LiveRevisions::default(), b"do-work", 1)
        .unwrap();
    let run_id = admitted.verified.spec().run_id.clone();
    let preq = admitted.verified.spec().provider_request_id.clone();
    let starting = admission
        .persist_starting(&run_id, admitted.revision)
        .unwrap();
    assert_eq!(starting, ExecutionLifecycle::Starting);
    let running = admission
        .persist_running(&run_id, admitted.revision + 1)
        .unwrap();
    assert_eq!(running, ExecutionLifecycle::Running);
    admission.begin_send(&preq, 0).unwrap();
    admission.mark_sent(&preq, 1).unwrap();
    assert_eq!(
        admission.recover_send(&preq).unwrap(),
        SendRecovery::AlreadySettled
    );
}

#[test]
fn sending_crash_is_uncertain_and_cannot_auto_retry() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let spec = unsigned_provider_spec("unc", "do-work")
        .seal(&key())
        .unwrap();
    let admitted = admission
        .admit(&key(), spec, LiveRevisions::default(), b"do-work", 1)
        .unwrap();
    let run_id = admitted.verified.spec().run_id.clone();
    let preq = admitted.verified.spec().provider_request_id.clone();
    admission
        .persist_starting(&run_id, admitted.revision)
        .unwrap();
    admission.begin_send(&preq, 0).unwrap();
    admission
        .mark_send_uncertain(&preq, 1, ProviderSendState::Sending)
        .unwrap();
    assert_eq!(
        admission.auto_retry_allowed(&preq).unwrap_err(),
        SpineError::AutoRetryForbidden
    );
    assert_eq!(
        admission.recover_send(&preq).unwrap(),
        SendRecovery::UncertainNoRetry
    );
}

#[test]
fn stale_lease_holder_is_denied() {
    let dir = tempdir().unwrap();
    let persist = SpinePersist::open(dir.path()).unwrap();
    let spec = unsigned_provider_spec("lease", "do-work")
        .seal(&key())
        .unwrap();
    let admission = DurableAdmission::new(persist.clone());
    admission
        .admit(
            &key(),
            spec.clone(),
            LiveRevisions::default(),
            b"do-work",
            1,
        )
        .unwrap();
    let lease = persist.load_lease(&spec.lease_id).unwrap();
    assert_eq!(
        lease.require_holder("other-owner", 1),
        Err(SpineError::StaleRevision)
    );
    assert_eq!(
        lease.require_unexpired(spec.lease_expiry_unix_ms),
        Err(SpineError::StaleRevision)
    );
}

#[test]
fn policy_revision_drift_denies_admission() {
    let dir = tempdir().unwrap();
    let admission = DurableAdmission::new(SpinePersist::open(dir.path()).unwrap());
    let spec = unsigned_provider_spec("pol", "do-work")
        .seal(&key())
        .unwrap();
    let live = LiveRevisions {
        policy: Revision::new(2),
        ..LiveRevisions::default()
    };
    assert_eq!(
        admission
            .admit(&key(), spec, live, b"do-work", 1)
            .unwrap_err(),
        SpineError::StaleRevision
    );
}

#[test]
fn terminal_replay_after_tombstone_does_not_create_a_second_run() {
    let dir = tempdir().unwrap();
    let persist = SpinePersist::open(dir.path()).unwrap();
    let admission = DurableAdmission::new(persist.clone());
    let spec = unsigned_provider_spec("tomb", "do-work")
        .seal(&key())
        .unwrap();
    let first = admission
        .admit(
            &key(),
            spec.clone(),
            LiveRevisions::default(),
            b"do-work",
            1,
        )
        .unwrap();
    let second = admission
        .replay_or_admit(&key(), spec, LiveRevisions::default(), b"do-work", 2)
        .unwrap();
    assert_eq!(first.verified.spec().run_id, second.verified.spec().run_id);
    assert!(persist
        .load_tombstone(&first.verified.spec().request_id)
        .unwrap()
        .is_some());
}
