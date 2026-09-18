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

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_uncertain_rejects_auto_retry_and_destroyed_after_stop() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser permitted");
    harness.boot().expect("boot");
    harness.schedule_uncertain_on_next_inject();
    harness
        .inject_guest_action(SyntheticGuestAction::ClickGuestButton)
        .expect_err("uncertain");
    let retry = harness
        .retry_inject_after_uncertain(SyntheticGuestAction::ClickGuestButton)
        .expect_err("no auto-retry");
    assert_eq!(retry.code, HarnessErrorCode::AutoRetryForbidden);

    let evidence = harness.stop().expect("stop");
    assert_eq!(harness.lifecycle().phase, GuestLifecyclePhase::Destroyed);
    assert!(evidence.destroy_confirmed(harness.lifecycle().phase));
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}

#[cfg(not(feature = "browser-engine"))]
#[test]
fn contained_browser_owned_page_navigation_and_host_input_refused() {
    use grokptah_isolated_surface::{
        ActionChannel, ContainedBrowserBackend, FrameKind, GuestLocalAction,
        IsolatedSurfaceBackend, OWNED_PAGE_URL,
    };

    let mut backend = ContainedBrowserBackend::new();
    backend.boot().expect("boot");
    assert_eq!(backend.current_page(), Some(OWNED_PAGE_URL));
    let first_store = backend
        .website_data_store_id()
        .expect("store minted")
        .to_string();
    backend
        .navigate("https://evil.example/")
        .expect_err("off-allowlist");
    backend
        .navigate("https://grokptah.owned.invalid/other")
        .expect_err("same-origin other path");
    backend.navigate(OWNED_PAGE_URL).expect("owned page");

    let err = backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::BlankTarget,
            ActionChannel::MainFrameDom,
        )
        .expect_err("_blank");
    assert_eq!(
        err.code,
        grokptah_isolated_surface::HarnessErrorCode::InvalidState
    );
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::SecondaryWindow,
            ActionChannel::MainFrameDom,
        )
        .expect_err("secondary");
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::HostKeyboard,
        )
        .expect_err("keyboard");
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::HostPointer,
        )
        .expect_err("pointer");
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::HostClipboard,
        )
        .expect_err("clipboard");
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::MainFrameDom,
        )
        .expect("main-frame DOM");

    backend.stop_fence_first().expect("fence");
    let fenced = backend
        .inject_guest_local(GuestLocalAction::ClickGuestButton)
        .expect_err("inject after fence");
    assert_eq!(
        fenced.code,
        grokptah_isolated_surface::HarnessErrorCode::InjectFenced
    );
    backend.destroy().expect("destroy");
    assert!(backend.website_data_store_id().is_none());
    backend.boot().expect("second boot");
    let second_store = backend.website_data_store_id().expect("fresh store");
    assert_ne!(first_store, second_store);
    assert!(!grokptah_isolated_surface::isolated_surface_admission_available());
}
