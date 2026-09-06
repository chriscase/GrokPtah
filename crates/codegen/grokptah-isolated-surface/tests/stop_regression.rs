//! Crash/restart/Stop regression suite for the Isolated Surface Proof Harness.

use grokptah_isolated_surface::{
    GuestLifecycleDisposition, GuestLifecyclePhase, HarnessErrorCode, HostSentinelRegistry,
    HostSentinelSnapshot, IsolatedSurfaceHarness, ProofEvidenceClass, SyntheticGuestAction,
};
use tempfile::TempDir;

fn harness_with_snapshot() -> (IsolatedSurfaceHarness, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline())
        .with_snapshot_root(dir.path());
    (harness, dir)
}

#[test]
fn canonical_proof_sequence_succeeds_with_unchanged_host_sentinels() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::SyntheticHarnessIneligible
    );

    let evidence = harness.run_canonical_proof().expect("canonical proof");
    assert!(evidence.host_sentinels_unchanged);
    assert!(evidence.host_sentinel_probe_error.is_none());
    assert!(harness.sentinels().verified_via_probe());
    assert_eq!(evidence.channels_destroyed, 2);
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    harness.sentinels().assert_unchanged().expect("sentinels");
}

#[test]
fn stop_evidence_rejects_skipped_host_probe() {
    let registry = HostSentinelRegistry::capture(HostSentinelSnapshot::synthetic_baseline());
    assert!(!registry.verified_via_probe());
    registry.assert_unchanged().expect_err("probe required");
}

#[test]
fn host_mutation_still_fences_and_tears_down_before_probe_error() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    assert_eq!(harness.channels().open_count(), 2);
    harness.host_probe_mut().simulate_host_mutation("pointer");

    let evidence = harness
        .stop()
        .expect("stop completes despite probe failure");
    assert!(!evidence.host_sentinels_unchanged);
    let probe_err = evidence
        .host_sentinel_probe_error
        .expect("probe error reported separately");
    assert_eq!(probe_err.code, HarnessErrorCode::HostSentinelViolation);

    assert!(harness.lifecycle().inject_fenced);
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    assert!(!harness.guest_is_booted());
    assert_eq!(evidence.channels_destroyed, 2);
    harness.channels().assert_all_destroyed().expect("channels");

    let inject_err = harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("inject fenced after stop");
    assert_eq!(inject_err.code, HarnessErrorCode::InjectFenced);
}

#[test]
fn stop_is_authoritative_and_fences_inject() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect("inject");

    let evidence = harness.stop().expect("stop");
    assert_eq!(
        evidence.disposition,
        Some(GuestLifecycleDisposition::Stopped)
    );
    assert!(evidence.host_sentinel_probe_error.is_none());
    let inject_err = harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("inject after stop");
    assert_eq!(inject_err.code, HarnessErrorCode::InjectFenced);
}

#[test]
fn crash_during_inject_marks_uncertain_without_auto_retry() {
    let (mut harness, _dir) = harness_with_snapshot();
    harness.boot().expect("boot");
    harness.schedule_crash_on_next_inject();

    let err = harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("crash inject");
    assert_eq!(err.code, HarnessErrorCode::UncertainOutcome);
    assert!(harness.lifecycle().inject_fenced);

    let retry_err = harness
        .retry_inject_after_uncertain(SyntheticGuestAction::ClickGuestButton)
        .expect_err("retry forbidden");
    assert_eq!(retry_err.code, HarnessErrorCode::AutoRetryForbidden);
    assert_eq!(harness.auto_retry_attempts(), 1);
}

#[test]
fn restart_after_uncertain_lands_destroyed_with_closed_channels() {
    let (mut harness, dir) = harness_with_snapshot();
    harness.boot().expect("boot");
    harness.schedule_uncertain_on_next_inject();
    harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("uncertain inject");

    let mut restarted = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline())
        .with_snapshot_root(dir.path());
    restarted.recover_after_restart().expect("recover");

    assert_eq!(restarted.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    assert_eq!(
        restarted.lifecycle().disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );
    assert!(!restarted.guest_is_booted());
    restarted
        .channels()
        .assert_all_destroyed()
        .expect("channels");
    assert!(restarted.lifecycle().inject_fenced);

    let retry_err = restarted
        .retry_inject_after_uncertain(SyntheticGuestAction::ClickGuestButton)
        .expect_err("no auto retry");
    assert_eq!(retry_err.code, HarnessErrorCode::AutoRetryForbidden);
}

#[test]
fn destroy_cleans_channels_and_preserves_host_sentinels() {
    let mut harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
    harness.boot().expect("boot");
    assert_eq!(harness.channels().open_count(), 2);

    let evidence = harness.stop().expect("stop");
    assert_eq!(evidence.channels_destroyed, 2);
    harness.channels().assert_all_destroyed().expect("no leak");
    harness.sentinels().assert_unchanged().expect("sentinels");
}

#[test]
fn stop_after_uncertain_still_tears_down_cleanly() {
    let (mut harness, _dir) = harness_with_snapshot();
    harness.boot().expect("boot");
    harness.schedule_uncertain_on_next_inject();
    harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("uncertain");

    let evidence = harness.stop().expect("stop after uncertain");
    assert_eq!(
        evidence.disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );
    harness.channels().assert_all_destroyed().expect("channels");
    harness.sentinels().assert_unchanged().expect("sentinels");
}
