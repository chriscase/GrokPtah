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
        let first_store_key = backend
            .live_wk_website_data_store_object_key()
            .expect("first run store object key");
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
            .expect("WK cancelled off-allowlist; owned page stays");
        let wk_url = backend
            .live_wk_current_url()
            .expect("WK current_url after cancelled navigation");
        assert!(
            wk_url.starts_with(OWNED_PAGE_URL) || wk_url == OWNED_PAGE_URL,
            "WK current_url must remain the owned page, got {wk_url}"
        );
        let (decided_url, policy) = backend
            .last_wk_navigation_decision()
            .expect("WKNavigationDelegate must fire a decision");
        assert_eq!(
            policy, 0,
            "off-allowlist must be WK policy cancel, got {policy}"
        );
        assert!(
            decided_url.contains("evil.example"),
            "cancel decision must be for the requested URL, got {decided_url}"
        );
        assert_live_wk_native_denies(&mut backend);
        assert_admitted_main_frame_inject_mutates_digest(&mut backend);
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
        assert!(
            !second
                .live_wk_uses_default_website_data_store()
                .expect("second run default store check"),
            "second run must not attach the default/profile store"
        );
        let first_key = first_store_key;
        let second_key = second
            .live_wk_website_data_store_object_key()
            .expect("second run store object key");
        assert_ne!(
            first_key, second_key,
            "each run must construct a distinct WKWebsiteDataStore"
        );
        assert!(
            second
                .live_wk_read_local_storage("cb-v0-store-probe")
                .expect("second-run localStorage")
                .is_none(),
            "fresh nonpersistent store must not import the previous run's localStorage"
        );
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
fn assert_live_wk_native_denies(backend: &mut grokptah_isolated_surface::ContainedBrowserBackend) {
    use grokptah_isolated_surface::{
        isolated_surface_admission_available, NativeDenyKind, OWNED_PAGE_URL,
    };

    backend
        .live_wk_write_local_storage("cb-v0-store-probe", "run-1")
        .expect("write localStorage into this run's nonpersistent store");
    assert_eq!(
        backend
            .live_wk_read_local_storage("cb-v0-store-probe")
            .expect("read localStorage"),
        Some("run-1".into())
    );
    assert!(
        !backend
            .live_wk_uses_default_website_data_store()
            .expect("default store check"),
        "live WK must not use the default/profile website-data store"
    );
    backend
        .live_wk_attempt_window_open()
        .expect("window.open denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::WindowOpen),
        "window.open must be a WK createWebView WindowOpen deny, not a bundled Popup/NewWindow record"
    );
    backend.live_wk_attempt_popup().expect("popup denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::Popup),
        "popup must be a WK createWebView Popup deny from windowFeatures"
    );
    backend
        .live_wk_attempt_new_window()
        .expect("_blank/new window denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::NewWindow),
        "_blank must be a WK navigation-policy NewWindow deny"
    );
    let (blank_url, blank_policy) = backend
        .last_wk_navigation_decision()
        .expect("WKNavigationDelegate must cancel the _blank probe");
    assert_eq!(blank_policy, 0);
    assert!(
        blank_url.contains("evil.example"),
        "_blank cancel decision must be for the probe URL, got {blank_url}"
    );
    backend.live_wk_attempt_download().expect("download denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::Download)
    );
    let (dl_url, dl_policy) = backend
        .last_wk_navigation_decision()
        .expect("WKNavigationDelegate must cancel the download probe");
    assert_eq!(dl_policy, 0);
    assert!(
        dl_url.contains("deny.bin"),
        "download cancel decision must be for deny.bin, got {dl_url}"
    );
    backend
        .live_wk_attempt_file_picker()
        .expect("file picker denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::FilePicker)
    );
    let (file_dirs, file_urls_null, file_wk) = backend
        .last_wk_open_panel_deny()
        .expect("WK runOpenPanel must fire for the file picker");
    assert!(
        !file_dirs,
        "file picker WKOpenPanelParameters must not allow directories"
    );
    assert!(file_urls_null, "file picker must complete with nil URLs");
    assert!(
        file_wk,
        "file picker must be WK-originated WKOpenPanelParameters, not an attached-UIDelegate IMP poke"
    );
    backend
        .live_wk_attempt_directory_picker()
        .expect("directory picker denied");
    assert_eq!(
        backend.last_wk_native_deny(),
        Some(NativeDenyKind::DirectoryPicker)
    );
    let (dir_dirs, dir_urls_null, dir_wk) = backend
        .last_wk_open_panel_deny()
        .expect("WK runOpenPanel must fire for the directory picker");
    assert!(
        dir_dirs,
        "directory picker WKOpenPanelParameters must allow directories"
    );
    assert!(
        dir_urls_null,
        "directory picker must complete with nil URLs"
    );
    assert!(
        dir_wk,
        "directory picker must be WK-originated WKOpenPanelParameters, not an attached-UIDelegate IMP poke"
    );
    println!("ok: live WK open-panel deny is WK-originated (file+directory, nil URLs)");
    let wk_url = backend
        .live_wk_current_url()
        .expect("owned page after native denies");
    assert!(
        wk_url.starts_with(OWNED_PAGE_URL) || wk_url == OWNED_PAGE_URL,
        "native denies must not navigate off the owned page, got {wk_url}"
    );
    assert!(!isolated_surface_admission_available());
    println!("ok: live WK native deny delegates + fresh nonpersistent store");
}

#[cfg(feature = "browser-engine")]
fn assert_admitted_main_frame_inject_mutates_digest(
    backend: &mut grokptah_isolated_surface::ContainedBrowserBackend,
) {
    use grokptah_isolated_surface::{
        ActionChannel, CapturedFrameSource, FrameKind, GuestLocalAction, InjectOutcome,
        IsolatedSurfaceBackend,
    };

    let before = backend
        .observe_frame()
        .expect("observe before admitted main-frame DOM inject");
    let original_epoch = before.epoch;
    let original_digest = before.digest.clone();
    let outcome = backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::MainFrameDom,
        )
        .expect("admitted live WK inject after capture must succeed");
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
    println!(
        "ok: live WK DOM inject guest_local_change={} before={} after={}",
        delta.guest_local_change, delta.before_digest, delta.after_digest
    );
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
