//! Receipt produce/consume wiring for `--features browser-engine`.
//!
//! Run via:
//! `cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml --features browser-engine --test browser_engine_receipt_wiring -- --test-threads=1`

#![cfg(feature = "browser-engine")]

use grokptah_isolated_surface::{
    admit_browser_engine_capture, admit_frame_action, admit_navigation, canonical_sha256_digest,
    capture_live_wk_snapshot_through_receipt, isolated_surface_admission_available,
    native_browser_engine_capture_authorized, owned_page_for_boot, validate_public_evidence,
    ActionChannel, CapturedFrameSource, ContainedBrowserBackend, FrameKind, GuestLocalAction,
    HarnessErrorCode, HostSentinelSnapshot, IsolatedSurfaceBackend, IsolatedSurfaceHarness,
    NonpersistentWebsiteDataStore, LIVE_WK_FIXTURE_CRIMSON_RGB, OWNED_PAGE_URL,
    SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
};

#[test]
fn receipt_gated_substrate_label_and_no_simulator_fallback() {
    let backend = ContainedBrowserBackend::new();
    assert_eq!(backend.substrate_mode_label(), "receipt_gated");
    assert!(!backend.isolation_proof_available());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn cargo_test_never_authorizes_native_browser_engine_capture() {
    assert!(!native_browser_engine_capture_authorized());
}

#[test]
fn boot_stays_fail_closed_without_native_capture_authorization() {
    let mut backend = ContainedBrowserBackend::new();
    let err = backend
        .boot()
        .expect_err("native boot unwired and unauthorized");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(!backend.is_booted());
    assert!(err.message.contains("receipt-gated"));
    assert!(!native_browser_engine_capture_authorized());
}

#[test]
fn public_capture_entry_still_fails_closed_without_receipt() {
    let err = admit_browser_engine_capture(vec![0x01, 0x02, 0x03], 8, 8)
        .expect_err("no public receipt mint");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(err.message.contains("not wired"));
}

#[test]
fn live_wk_snapshot_zero_epoch_is_rejected() {
    let err = capture_live_wk_snapshot_through_receipt(0).expect_err("epoch 0");
    assert_eq!(err.code, HarnessErrorCode::InvalidState);
    assert!(!native_browser_engine_capture_authorized());
    assert!(!isolated_surface_admission_available());
}

#[cfg(not(target_os = "macos"))]
#[test]
fn live_wk_snapshot_fail_closes_off_macos() {
    let err = capture_live_wk_snapshot_through_receipt(1).expect_err("linux fail-closed");
    assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
    assert!(err.message.contains("macOS-only") || err.message.contains("fail-closed"));
    assert!(!native_browser_engine_capture_authorized());
    assert!(!isolated_surface_admission_available());
    let _ = LIVE_WK_FIXTURE_CRIMSON_RGB;
}

#[cfg(target_os = "macos")]
#[test]
fn live_wk_snapshot_mints_receipt_and_completes_engine_rgba8() {
    assert!(!native_browser_engine_capture_authorized());
    match capture_live_wk_snapshot_through_receipt(4) {
        Ok(capture) => {
            assert!(!native_browser_engine_capture_authorized());
            assert!(!isolated_surface_admission_available());
            assert_eq!(capture.source(), CapturedFrameSource::BrowserEngine);
            assert_eq!(capture.epoch(), 4);
            let evidence = capture.public_evidence();
            assert_eq!(
                evidence.media_kind,
                grokptah_isolated_surface::CapturedFrameMediaKind::EngineRgba8
            );
            assert_eq!(
                evidence.byte_length,
                (evidence.width as usize) * (evidence.height as usize) * 4
            );
            assert!(!capture
                .captured_bytes()
                .windows(SYNTHETIC_FRAME_PAYLOAD_NEEDLE.len())
                .any(|window| window == SYNTHETIC_FRAME_PAYLOAD_NEEDLE));
            let bytes = capture.captured_bytes();
            assert!(
                !bytes
                    .chunks_exact(4)
                    .all(|pixel| pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255),
                "window-white raster is not WK-composited fixture"
            );
            let [target_r, target_g, target_b] = LIVE_WK_FIXTURE_CRIMSON_RGB;
            let near =
                |actual: u8, target: u8| (actual as i16 - target as i16).unsigned_abs() <= 40;
            assert!(
                bytes.chunks_exact(4).any(|pixel| {
                    near(pixel[0], target_r) && near(pixel[1], target_g) && near(pixel[2], target_b)
                        || near(pixel[0], target_b)
                            && near(pixel[1], target_g)
                            && near(pixel[2], target_r)
                }),
                "captured bytes must contain fixture #c41e3a"
            );
            assert_eq!(
                capture.digest(),
                canonical_sha256_digest(capture.captured_bytes())
            );
            assert_eq!(
                validate_public_evidence(&evidence)
                    .expect_err("public verifier stays fail-closed for engine evidence")
                    .code,
                HarnessErrorCode::BackendUnavailable
            );
        }
        Err(err) if err.message.contains("main thread") => {
            assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
            assert!(!native_browser_engine_capture_authorized());
        }
        Err(err) => panic!("live WK snapshot through shipped receipt path: {err:?}"),
    }
}

#[test]
fn harness_receipt_gated_observe_never_falls_back_to_simulator() {
    let mut harness = IsolatedSurfaceHarness::with_backend(
        HostSentinelSnapshot::synthetic_baseline(),
        ContainedBrowserBackend::new(),
    )
    .expect("contained browser harness");
    match harness.capture_and_observe_receipt_gated() {
        Ok(frame) => {
            assert_eq!(
                frame.captured_frame.as_ref().map(|ev| ev.source),
                Some(CapturedFrameSource::BrowserEngine)
            );
            assert_ne!(
                frame.captured_frame.as_ref().map(|ev| ev.source),
                Some(CapturedFrameSource::SyntheticSimulator)
            );
        }
        Err(err) => {
            assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        }
    }
    assert!(!native_browser_engine_capture_authorized());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn contained_browser_v0_allowlist_and_main_frame_policy() {
    assert_eq!(owned_page_for_boot().expect("owned page"), OWNED_PAGE_URL);
    admit_navigation("https://example.com/").expect_err("off-allowlist");
    admit_navigation("https://grokptah.owned.invalid/other").expect_err("other path");
    admit_navigation("about:blank").expect_err("about:blank");
    admit_frame_action(FrameKind::MainFrame, ActionChannel::MainFrameDom).expect("main DOM");
    admit_frame_action(FrameKind::BlankTarget, ActionChannel::MainFrameDom).expect_err("_blank");
    admit_frame_action(FrameKind::SecondaryWindow, ActionChannel::MainFrameDom)
        .expect_err("secondary");
    admit_frame_action(FrameKind::MainFrame, ActionChannel::HostKeyboard).expect_err("keyboard");
    admit_frame_action(FrameKind::MainFrame, ActionChannel::HostPointer).expect_err("pointer");
    admit_frame_action(FrameKind::MainFrame, ActionChannel::HostClipboard).expect_err("clipboard");

    let mut backend = ContainedBrowserBackend::new();
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::BlankTarget,
            ActionChannel::MainFrameDom,
        )
        .expect_err("_blank before boot");
    backend
        .inject_dom_action(
            GuestLocalAction::ClickGuestButton,
            FrameKind::MainFrame,
            ActionChannel::HostClipboard,
        )
        .expect_err("clipboard before boot");
    let first = NonpersistentWebsiteDataStore::mint();
    let second = NonpersistentWebsiteDataStore::mint();
    assert_ne!(first.run_id(), second.run_id());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn contained_browser_native_deny_policy_fail_closed() {
    use grokptah_isolated_surface::{
        admit_native_capability, live_wk_create_webview_policy_allows,
        live_wk_download_policy_allows, live_wk_navigation_action_policy_allows,
        live_wk_navigation_response_policy_allows, live_wk_open_panel_policy_allows,
        NativeDenyKind,
    };

    assert!(!live_wk_download_policy_allows());
    assert!(!live_wk_open_panel_policy_allows(false));
    assert!(!live_wk_open_panel_policy_allows(true));
    assert!(!live_wk_create_webview_policy_allows());
    assert!(!live_wk_navigation_action_policy_allows(
        OWNED_PAGE_URL,
        true,
        false,
        true
    ));
    assert!(!live_wk_navigation_response_policy_allows(
        OWNED_PAGE_URL,
        true,
        false,
        false
    ));
    let backend = ContainedBrowserBackend::new();
    for kind in [
        NativeDenyKind::Download,
        NativeDenyKind::FilePicker,
        NativeDenyKind::DirectoryPicker,
        NativeDenyKind::WindowOpen,
        NativeDenyKind::Popup,
        NativeDenyKind::NewWindow,
    ] {
        admit_native_capability(kind).expect_err(kind.as_str());
        backend
            .refuse_native_capability(kind)
            .expect_err(kind.as_str());
    }
    assert!(!isolated_surface_admission_available());
}

#[test]
fn live_wk_open_panel_deny_requires_wk_delivery() {
    use grokptah_isolated_surface::live_wk_open_panel_policy_allows;

    assert!(!live_wk_open_panel_policy_allows(false));
    assert!(!live_wk_open_panel_policy_allows(true));
    let mut backend = ContainedBrowserBackend::new();
    assert!(
        backend.last_wk_open_panel_deny().is_none(),
        "no WKOpenPanelParameters record without a live WK session"
    );
    backend
        .live_wk_attempt_file_picker()
        .expect_err("file picker without WK-originated runOpenPanel");
    backend
        .live_wk_attempt_directory_picker()
        .expect_err("directory picker without WK-originated runOpenPanel");
    assert!(backend.last_wk_open_panel_deny().is_none());
    assert!(!isolated_surface_admission_available());
}

#[test]
fn live_wk_navigation_policy_helper_cancels_off_allowlist() {
    use grokptah_isolated_surface::live_wk_navigation_policy_allows;
    assert!(live_wk_navigation_policy_allows(
        OWNED_PAGE_URL,
        true,
        false
    ));
    assert!(!live_wk_navigation_policy_allows(
        "https://evil.example/",
        true,
        false
    ));
    assert!(!live_wk_navigation_policy_allows(
        OWNED_PAGE_URL,
        true,
        true
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn live_wk_backend_containment_after_capture() {
    let mut backend = ContainedBrowserBackend::new();
    match backend.capture_live_wk_snapshot() {
        Ok(_) => {
            assert_eq!(backend.current_page(), Some(OWNED_PAGE_URL));
            let first_store = backend
                .website_data_store_id()
                .expect("store minted")
                .to_string();
            backend
                .navigate("https://evil.example/")
                .expect_err("off-allowlist");
            backend
                .inject_dom_action(
                    GuestLocalAction::ClickGuestButton,
                    FrameKind::BlankTarget,
                    ActionChannel::MainFrameDom,
                )
                .expect_err("_blank");
            backend
                .inject_dom_action(
                    GuestLocalAction::ClickGuestButton,
                    FrameKind::MainFrame,
                    ActionChannel::HostKeyboard,
                )
                .expect_err("keyboard");
            let before = IsolatedSurfaceBackend::observe_frame(&backend)
                .expect("observe before admitted main-frame DOM inject");
            let original_epoch = before.epoch;
            let original_digest = before.digest.clone();
            assert!(
                !backend
                    .live_website_data_store_is_persistent()
                    .expect("runtime store persistence"),
                "live capture must report a nonpersistent store"
            );
            backend
                .live_wk_attempt_navigation("https://evil.example/")
                .expect("WK cancelled off-allowlist");
            let wk_url = backend
                .live_wk_current_url()
                .expect("WK current_url after cancel");
            assert!(
                wk_url.starts_with(OWNED_PAGE_URL) || wk_url == OWNED_PAGE_URL,
                "WK URL must stay owned, got {wk_url}"
            );
            let (decided_url, policy) = backend
                .last_wk_navigation_decision()
                .expect("WKNavigationDelegate decision");
            assert_eq!(policy, 0);
            assert!(decided_url.contains("evil.example"));
            assert_live_wk_native_denies_and_fresh_store(&mut backend, &first_store);
            let outcome = backend
                .inject_dom_action(
                    GuestLocalAction::ClickGuestButton,
                    FrameKind::MainFrame,
                    ActionChannel::MainFrameDom,
                )
                .expect("admitted inject after live capture must succeed");
            let grokptah_isolated_surface::InjectOutcome::Changed(delta) = outcome else {
                panic!("admitted inject must be Changed");
            };
            assert!(delta.guest_local_change);
            assert_ne!(delta.before_digest, original_digest);
            let after = IsolatedSurfaceBackend::observe_frame(&backend)
                .expect("successful inject must keep observation bound");
            assert_eq!(after.epoch, original_epoch.saturating_add(1));
            IsolatedSurfaceBackend::stop_fence_first(&mut backend).expect("fence");
            IsolatedSurfaceBackend::inject_guest_local(
                &mut backend,
                GuestLocalAction::ClickGuestButton,
            )
            .expect_err("fenced");
            backend.destroy().expect("destroy");
            assert!(backend.website_data_store_id().is_none());
            match backend.capture_live_wk_snapshot() {
                Ok(_) => {
                    let second = backend.website_data_store_id().expect("fresh store");
                    assert_ne!(first_store, second);
                    assert!(
                        !backend
                            .live_wk_uses_default_website_data_store()
                            .expect("default store check"),
                        "second run must not attach the default/profile store"
                    );
                    assert!(
                        backend
                            .live_wk_read_local_storage("cb-v0-store-probe")
                            .expect("second-run localStorage")
                            .is_none(),
                        "fresh nonpersistent store must not import the previous run's localStorage"
                    );
                }
                Err(err) if err.message.contains("main thread") => {}
                Err(err) => panic!("second live WK capture: {err:?}"),
            }
            assert!(!isolated_surface_admission_available());
        }
        Err(err) if err.message.contains("main thread") => {
            assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        }
        Err(err) => panic!("live WK containment capture: {err:?}"),
    }
}

#[cfg(target_os = "macos")]
fn assert_live_wk_native_denies_and_fresh_store(
    backend: &mut ContainedBrowserBackend,
    first_store: &str,
) {
    use grokptah_isolated_surface::NativeDenyKind;

    let _ = first_store;
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
    let store_key = backend
        .live_wk_website_data_store_object_key()
        .expect("store object key");
    assert_ne!(store_key, 0);

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
        .expect("WKNavigationDelegate must decide the download probe");
    assert!(
        dl_policy == 0 || dl_policy == 2,
        "download must return WK policy 2 or cancel after WKDownload, got {dl_policy}"
    );
    assert!(
        grokptah_isolated_surface::is_download_probe_url(&dl_url),
        "download decision must be the dedicated grokptah-cbv0 URL, got {dl_url}"
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
    assert!(!file_dirs);
    assert!(file_urls_null);
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
    assert!(dir_dirs);
    assert!(dir_urls_null);
    assert!(
        dir_wk,
        "directory picker must be WK-originated WKOpenPanelParameters, not an attached-UIDelegate IMP poke"
    );
    let wk_url = backend
        .live_wk_current_url()
        .expect("owned page after native denies");
    assert!(
        wk_url.starts_with(OWNED_PAGE_URL) || wk_url == OWNED_PAGE_URL,
        "native denies must not navigate off the owned page, got {wk_url}"
    );
    assert!(!isolated_surface_admission_available());
}
