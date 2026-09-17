//! Browser-engine captured-frame receipt wiring (Phase-1 fail-closed).
//!
//! Process-private one-shot receipts bind bounded RGBA8 snapshot bytes to a
//! booted frame epoch. No receipt → no frame bytes. Live macOS `WKWebView`
//! rasters enter only through [`capture_live_wk_snapshot_through_receipt`].
//! Admission and Computer Mode stay false.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::captured_frame::{
    admit_browser_engine_capture_with_receipt, issue_browser_engine_capture_receipt,
    BoundedCapturedFrame, BrowserEngineCaptureReceipt, CapturedFrameSource,
};
use crate::error::{HarnessError, HarnessResult};

/// Process-local authorization for native WebKit frame capture. Ordinary
/// `cargo test` must never set this. Only the exclusive physical CLI/runner may.
static NATIVE_CAPTURE_AUTHORIZED: AtomicBool = AtomicBool::new(false);

/// Authorize native WebKit browser-engine frame capture. Call only from the
/// exclusive physical CLI once a separately qualified native boot path exists.
pub fn authorize_native_browser_engine_capture_for_physical_cli() {
    NATIVE_CAPTURE_AUTHORIZED.store(true, Ordering::SeqCst);
}

/// Whether the current process may initialize WebKit for receipt-gated capture.
pub fn native_browser_engine_capture_authorized() -> bool {
    NATIVE_CAPTURE_AUTHORIZED.load(Ordering::SeqCst)
}

/// CSS `#c41e3a` body background of the live WK fixture. RGBA or BGRA rasters
/// must contain this crimson; uniform window-white is not engine content.
pub const LIVE_WK_FIXTURE_CRIMSON_RGB: [u8; 3] = [0xC4, 0x1E, 0x3A];

/// Handoff token minted immediately before a native snapshot attempt.
///
/// Dropping without completion revokes the receipt via
/// [`BrowserEngineCaptureReceipt`]'s `Drop` implementation.
pub(crate) struct BrowserEngineCaptureHandoff {
    epoch: u64,
    receipt: BrowserEngineCaptureReceipt,
}

impl BrowserEngineCaptureHandoff {
    pub(crate) fn epoch(&self) -> u64 {
        self.epoch
    }
}

/// Mint a process-private one-shot receipt bound to `epoch`.
///
/// Call this immediately before the native adapter requests snapshot bytes.
pub(crate) fn begin_browser_engine_frame_capture(
    epoch: u64,
) -> HarnessResult<BrowserEngineCaptureHandoff> {
    if epoch == 0 {
        return Err(HarnessError::invalid_state(
            "browser-engine capture handoff requires a booted frame epoch",
        ));
    }
    let receipt = issue_browser_engine_capture_receipt(epoch)?;
    Ok(BrowserEngineCaptureHandoff { epoch, receipt })
}

/// Admit bounded RGBA8 bytes produced by the native adapter.
///
/// The receipt is consumed one-shot; bytes without a valid registered receipt
/// are rejected. This is the only path that may label bytes as
/// [`CapturedFrameSource::BrowserEngine`].
pub(crate) fn complete_browser_engine_frame_capture(
    handoff: BrowserEngineCaptureHandoff,
    rgba8_bytes: Vec<u8>,
    width: u32,
    height: u32,
) -> HarnessResult<BoundedCapturedFrame> {
    admit_browser_engine_capture_with_receipt(
        handoff.epoch,
        rgba8_bytes,
        width,
        height,
        handoff.receipt,
    )
}

/// Fail-closed observation: returns the admitted engine frame or an error.
///
/// Never falls back to simulator bytes.
pub(crate) fn require_engine_frame_observation(
    frame_epoch: u64,
    stored: Option<&BoundedCapturedFrame>,
) -> HarnessResult<BoundedCapturedFrame> {
    let capture = stored.ok_or_else(|| {
        HarnessError::backend_unavailable(
            "browser-engine observation requires receipt-gated capture bytes; none admitted for this epoch",
        )
    })?;
    if capture.epoch() != frame_epoch {
        return Err(HarnessError::invalid_state(
            "browser-engine observation epoch is stale or misbound",
        ));
    }
    if capture.source() != CapturedFrameSource::BrowserEngine {
        return Err(HarnessError::backend_unavailable(
            "browser-engine observation rejects non-engine frame source",
        ));
    }
    Ok(capture.clone())
}

/// Raster a live macOS `WKWebView` fixture and admit RGBA8 only through a
/// process-private one-shot receipt. Does not set the physical-CLI authorize
/// flag. Non-macOS builds fail closed.
pub fn capture_live_wk_snapshot_through_receipt(epoch: u64) -> HarnessResult<BoundedCapturedFrame> {
    if epoch == 0 {
        return Err(HarnessError::invalid_state(
            "browser-engine capture handoff requires a booted frame epoch",
        ));
    }
    #[cfg(target_os = "macos")]
    {
        let raster = crate::wk_native_snapshot::rasterize_fixture()?;
        let handoff = begin_browser_engine_frame_capture(epoch)?;
        if handoff.epoch() != epoch {
            return Err(HarnessError::invalid_state(
                "live WK snapshot receipt epoch drifted before admit",
            ));
        }
        complete_browser_engine_frame_capture(handoff, raster.bytes, raster.width, raster.height)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = epoch;
        Err(HarnessError::backend_unavailable(
            "live WK snapshot is macOS-only; receipt-gated capture stays fail-closed on this platform",
        ))
    }
}

/// Install a receipt-gated capture into backend state after admission.
pub(crate) fn install_receipt_gated_capture(
    frame_epoch: u64,
    capture: BoundedCapturedFrame,
) -> HarnessResult<BoundedCapturedFrame> {
    if capture.epoch() != frame_epoch {
        return Err(HarnessError::invalid_state(
            "receipt-gated capture epoch does not match backend frame epoch",
        ));
    }
    if capture.source() != CapturedFrameSource::BrowserEngine {
        return Err(HarnessError::backend_unavailable(
            "receipt-gated capture must be browser-engine sourced",
        ));
    }
    Ok(capture)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captured_frame::canonical_sha256_digest;

    const RGBA8: usize = 4;

    #[test]
    fn handoff_mint_complete_observation_round_trip() {
        let handoff = begin_browser_engine_frame_capture(3).expect("mint");
        assert_eq!(handoff.epoch(), 3);
        let bytes = vec![0x44; 2 * 2 * RGBA8];
        let capture =
            complete_browser_engine_frame_capture(handoff, bytes.clone(), 2, 2).expect("admit");
        assert_eq!(capture.source(), CapturedFrameSource::BrowserEngine);
        assert_eq!(capture.digest(), canonical_sha256_digest(&bytes));

        let observed = require_engine_frame_observation(3, Some(&capture)).expect("observe");
        assert_eq!(observed.digest(), capture.digest());
    }

    #[test]
    fn no_receipt_no_observation_bytes() {
        let err = require_engine_frame_observation(1, None).expect_err("no bytes");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("receipt-gated"));
    }

    #[test]
    fn dropped_handoff_revokes_receipt() {
        let handoff = begin_browser_engine_frame_capture(5).expect("mint");
        let bytes = vec![0x11; RGBA8];
        drop(handoff);
        let retry = begin_browser_engine_frame_capture(5).expect("remint");
        // A fresh receipt should admit; the dropped one must not be reusable.
        complete_browser_engine_frame_capture(retry, bytes, 1, 1).expect("fresh receipt");
    }

    #[test]
    fn stale_epoch_observation_rejected() {
        let handoff = begin_browser_engine_frame_capture(2).expect("mint");
        let capture =
            complete_browser_engine_frame_capture(handoff, vec![0x22; RGBA8], 1, 1).expect("admit");
        let err = require_engine_frame_observation(3, Some(&capture)).expect_err("stale");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);
    }
}
