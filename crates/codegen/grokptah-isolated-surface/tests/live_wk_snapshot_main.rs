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
        validate_public_evidence, CapturedFrameMediaKind, CapturedFrameSource, HarnessErrorCode,
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
