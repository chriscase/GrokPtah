//! Contained Browser substrate regression — fence-first Stop + Uncertain invariants.

#[cfg(not(feature = "browser-engine"))]
use grokptah_isolated_surface::{
    ContainedBrowserBackend, GuestLifecycleDisposition, GuestLifecyclePhase, HarnessErrorCode,
    HostSentinelSnapshot, IsolatedSurfaceHarness, ProofEvidenceClass, SyntheticGuestAction,
};
#[cfg(not(feature = "browser-engine"))]
use tempfile::TempDir;

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_canonical_proof_with_unchanged_host_sentinels() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser permitted");
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );
    assert!(!harness.evidence_class().is_vf_qualification_eligible());

    let evidence = harness.run_canonical_proof().expect("canonical proof");
    assert!(evidence.host_sentinels_unchanged);
    assert_eq!(evidence.channels_destroyed, 2);
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    harness.sentinels().assert_unchanged().expect("sentinels");
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_stop_after_uncertain_preserves_disposition() {
    let dir = TempDir::new().expect("tempdir");
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser permitted")
    .with_snapshot_root(dir.path());
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
}

#[test]
fn contained_browser_admission_stays_false() {
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_post_stop_inject_fenced() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser permitted");
    harness.boot().expect("boot");
    harness.stop().expect("stop");

    let err = harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("inject after stop");
    assert_eq!(err.code, HarnessErrorCode::InjectFenced);
}
