//! Main-thread live WK snapshot through the shipped receipt mint → complete path.
//!
//! libtest worker threads are not the process main thread, so this file uses
//! `harness = false` and runs `fn main` on main. Not isolation PASS, not admission.

#[cfg(not(feature = "browser-engine"))]
fn main() {
    eprintln!("live_wk_snapshot_main requires --features browser-engine");
    std::process::exit(2);
}

#[cfg(feature = "browser-engine")]
fn main() {
    use grokptah_isolated_surface::{
        canonical_sha256_digest, capture_live_wk_snapshot_through_receipt,
        isolated_surface_admission_available, native_browser_engine_capture_authorized,
        validate_public_evidence, ActionChannel, CapturedFrameMediaKind, CapturedFrameSource,
        ContainedBrowserBackend, FrameKind, GuestLocalAction, HarnessErrorCode,
        HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness, OWNED_PAGE_URL,
        SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
    };

    assert!(
        !native_browser_engine_capture_authorized(),
        "ordinary test process must not latch the physical-CLI authorize flag"
    );
    assert!(!isolated_surface_admission_available());

    #[cfg(not(target_os = "macos"))]
    {
        let err = capture_live_wk_snapshot_through_receipt(1)
            .expect_err("live WK snapshot is macOS-only");
        assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        println!("ok: non-macos live WK snapshot fail-closed");
        return;
    }

    #[cfg(target_os = "macos")]
    {
        let capture = capture_live_wk_snapshot_through_receipt(7)
            .expect("main-thread live WK snapshot through shipped receipt path");
        assert!(!native_browser_engine_capture_authorized());
        assert!(!isolated_surface_admission_available());
        assert_eq!(capture.source(), CapturedFrameSource::BrowserEngine);
        assert_eq!(capture.epoch(), 7);
        let evidence = capture.public_evidence();
        assert_eq!(evidence.media_kind, CapturedFrameMediaKind::EngineRgba8);
        assert_eq!(
            evidence.byte_length,
            (evidence.width as usize) * (evidence.height as usize) * 4
        );
        assert!(!capture
            .captured_bytes()
            .windows(SYNTHETIC_FRAME_PAYLOAD_NEEDLE.len())
            .any(|window| window == SYNTHETIC_FRAME_PAYLOAD_NEEDLE));
        assert_captured_bytes_are_fixture_crimson(capture.captured_bytes());
        assert_eq!(
            capture.digest(),
            canonical_sha256_digest(capture.captured_bytes())
        );
        assert_eq!(
            validate_public_evidence(&evidence)
                .expect_err("public verifier stays fail-closed")
                .code,
            HarnessErrorCode::BackendUnavailable
        );
        println!(
            "ok: live WK receipt-gated capture {}x{} bytes={} digest={}",
            evidence.width, evidence.height, evidence.byte_length, evidence.digest
        );

        let mut backend = ContainedBrowserBackend::new();
        let backend_capture = backend
            .capture_live_wk_snapshot()
            .expect("backend capture_live_wk_snapshot through live WK seam");
        assert_eq!(backend_capture.source(), CapturedFrameSource::BrowserEngine);
        assert_captured_bytes_are_fixture_crimson(backend_capture.captured_bytes());
        let observed = backend
            .observe_frame()
            .expect("ReceiptGated observe uses stored live WK capture");
        assert_eq!(observed.digest, backend_capture.digest());
        assert_eq!(
            observed.captured_frame.as_ref().map(|ev| ev.source),
            Some(CapturedFrameSource::BrowserEngine)
        );
        assert_eq!(backend.current_page(), Some(OWNED_PAGE_URL));
        let first_store = backend
            .website_data_store_id()
            .expect("nonpersistent store minted")
            .to_string();
        backend
            .navigate("https://evil.example/")
            .expect_err("off-allowlist navigation denied");
        backend
            .navigate("https://grokptah.owned.invalid/other")
            .expect_err("same-origin other path denied");
        backend
            .inject_dom_action(
                GuestLocalAction::ClickGuestButton,
                FrameKind::BlankTarget,
                ActionChannel::MainFrameDom,
            )
            .expect_err("_blank refused");
        backend
            .inject_dom_action(
                GuestLocalAction::ClickGuestButton,
                FrameKind::SecondaryWindow,
                ActionChannel::MainFrameDom,
            )
            .expect_err("secondary window refused");
        backend
            .inject_dom_action(
                GuestLocalAction::ClickGuestButton,
                FrameKind::MainFrame,
                ActionChannel::HostKeyboard,
            )
            .expect_err("host keyboard refused");
        backend
            .inject_dom_action(
                GuestLocalAction::ClickGuestButton,
                FrameKind::MainFrame,
                ActionChannel::HostPointer,
            )
            .expect_err("host pointer refused");
        backend
            .inject_dom_action(
                GuestLocalAction::ClickGuestButton,
                FrameKind::MainFrame,
                ActionChannel::HostClipboard,
            )
            .expect_err("host clipboard refused");
        assert!(
            !backend
                .live_website_data_store_is_persistent()
                .expect("runtime store persistence"),
            "live WK must use a nonpersistent website-data store"
        );
        backend
            .live_wk_attempt_navigation("https://evil.example/")
            .expect_err("WK navigation policy must deny off-allowlist");
        assert_eq!(backend.current_page(), Some(OWNED_PAGE_URL));
        assert_admitted_main_frame_inject_does_not_desync(&mut backend);
        backend.stop_fence_first().expect("fence");
        let fenced = IsolatedSurfaceBackend::inject_guest_local(
            &mut backend,
            GuestLocalAction::ClickGuestButton,
        )
        .expect_err("inject after fence");
        assert_eq!(fenced.code, HarnessErrorCode::InjectFenced);
        backend.destroy().expect("destroy");
        assert!(!backend.is_booted());
        assert!(backend.website_data_store_id().is_none());
        backend
            .observe_frame()
            .expect_err("destroy unboots; observe cannot inherit stored capture");

        let mut second = ContainedBrowserBackend::new();
        let _ = second
            .capture_live_wk_snapshot()
            .expect("second run live WK snapshot");
        let second_store = second
            .website_data_store_id()
            .expect("second run mints a fresh store");
        assert_ne!(first_store, second_store);
        second.destroy().expect("destroy second");

        let mut harness = IsolatedSurfaceHarness::with_backend(
            HostSentinelSnapshot::synthetic_baseline(),
            ContainedBrowserBackend::new(),
        )
        .expect("contained browser harness");
        let harness_frame = harness
            .capture_and_observe_receipt_gated()
            .expect("harness ReceiptGated observe through live WK mint-complete");
        assert_eq!(
            harness_frame.captured_frame.as_ref().map(|ev| ev.source),
            Some(CapturedFrameSource::BrowserEngine)
        );
        assert_eq!(
            harness_frame
                .captured_frame
                .as_ref()
                .map(|ev| ev.media_kind),
            Some(CapturedFrameMediaKind::EngineRgba8)
        );
        assert!(!native_browser_engine_capture_authorized());
        assert!(!isolated_surface_admission_available());
        println!(
            "ok: ReceiptGated backend+harness observe digest={}",
            harness_frame.digest
        );

        harness.schedule_uncertain_on_next_inject();
        harness
            .inject_guest_action(GuestLocalAction::ClickGuestButton)
            .expect_err("uncertain");
        let retry = harness
            .retry_inject_after_uncertain(GuestLocalAction::ClickGuestButton)
            .expect_err("no auto-retry");
        assert_eq!(retry.code, HarnessErrorCode::AutoRetryForbidden);
        let evidence = harness.stop().expect("stop after uncertain");
        assert_eq!(
            harness.lifecycle().phase,
            grokptah_isolated_surface::GuestLifecyclePhase::Destroyed
        );
        assert!(evidence.destroy_confirmed(harness.lifecycle().phase));
        println!("ok: browser-engine Stop/Uncertain/Destroyed-after-confirm");
    }
}

#[cfg(feature = "browser-engine")]
fn assert_admitted_main_frame_inject_does_not_desync(
    backend: &mut grokptah_isolated_surface::ContainedBrowserBackend,
) {
    use grokptah_isolated_surface::{
        ActionChannel, CapturedFrameSource, FrameKind, GuestLocalAction, HarnessErrorCode,
        InjectOutcome, IsolatedSurfaceBackend,
    };

    let before = backend
        .observe_frame()
        .expect("observe before admitted main-frame DOM inject");
    let original_epoch = before.epoch;
    let original_digest = before.digest.clone();
    match backend.inject_dom_action(
        GuestLocalAction::ClickGuestButton,
        FrameKind::MainFrame,
        ActionChannel::MainFrameDom,
    ) {
        Ok(outcome) => {
            let delta = match outcome {
                InjectOutcome::Changed(delta) => delta,
                other => panic!("admitted live WK inject must be Changed, got {other:?}"),
            };
            assert!(
                delta.guest_local_change,
                "live WK main-frame DOM inject must change raster bytes"
            );
            assert_ne!(delta.before_digest, delta.after_digest);
            assert_eq!(delta.after_epoch, original_epoch.saturating_add(1));
            let after = backend
                .observe_frame()
                .expect("successful inject must keep observation bound to the new epoch");
            assert_eq!(after.epoch, original_epoch.saturating_add(1));
            assert_ne!(after.digest, original_digest);
            assert_eq!(
                after.captured_frame.as_ref().map(|ev| ev.source),
                Some(CapturedFrameSource::BrowserEngine)
            );
        }
        Err(err) => {
            assert_eq!(
                err.code,
                HarnessErrorCode::BackendUnavailable,
                "fail-closed inject must be BackendUnavailable without dirty epoch, got {err:?}"
            );
            let still = backend.observe_frame().expect(
                "fail-closed inject must not desync stored capture; observe at original epoch",
            );
            assert_eq!(still.epoch, original_epoch);
            assert_eq!(still.digest, original_digest);
        }
    }
}

#[cfg(feature = "browser-engine")]
fn assert_captured_bytes_are_fixture_crimson(bytes: &[u8]) {
    use grokptah_isolated_surface::LIVE_WK_FIXTURE_CRIMSON_RGB;
    assert!(!bytes.is_empty(), "live WK raster is empty");
    assert!(bytes.len().is_multiple_of(4), "live WK raster is not RGBA8");
    let uniform_white = bytes
        .chunks_exact(4)
        .all(|pixel| pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255);
    assert!(
        !uniform_white,
        "live WK raster is uniform 255,255,255,255 window-white, not fixture #c41e3a"
    );
    let [target_r, target_g, target_b] = LIVE_WK_FIXTURE_CRIMSON_RGB;
    let near = |actual: u8, target: u8| (actual as i16 - target as i16).unsigned_abs() <= 40;
    let has_fixture = bytes.chunks_exact(4).any(|pixel| {
        near(pixel[0], target_r) && near(pixel[1], target_g) && near(pixel[2], target_b)
            || near(pixel[0], target_b) && near(pixel[1], target_g) && near(pixel[2], target_r)
    });
    assert!(
        has_fixture,
        "live WK raster must contain fixture #c41e3a (RGBA or BGRA); first pixel={:?}",
        bytes.get(0..4)
    );
}
