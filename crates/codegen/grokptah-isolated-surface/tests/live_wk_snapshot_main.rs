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
