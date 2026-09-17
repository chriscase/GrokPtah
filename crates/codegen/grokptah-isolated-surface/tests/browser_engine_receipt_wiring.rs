//! Receipt produce/consume wiring for `--features browser-engine`.
//!
//! Run via:
//! `cargo test --locked --manifest-path crates/codegen/grokptah-isolated-surface/Cargo.toml --features browser-engine --test browser_engine_receipt_wiring -- --test-threads=1`

#![cfg(feature = "browser-engine")]

use grokptah_isolated_surface::{
    admit_browser_engine_capture, canonical_sha256_digest,
    capture_live_wk_snapshot_through_receipt, isolated_surface_admission_available,
    native_browser_engine_capture_authorized, validate_public_evidence, CapturedFrameSource,
    ContainedBrowserBackend, HarnessErrorCode, HostSentinelSnapshot, IsolatedSurfaceBackend,
    IsolatedSurfaceHarness, LIVE_WK_FIXTURE_CRIMSON_RGB, SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
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
