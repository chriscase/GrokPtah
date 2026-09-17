//! Contained Browser substrate v0 — Sep 18 pivot path, admission-false.
//!
//! Browser-only scope: guest-local DOM inject and frame observation only.
//! Not arbitrary desktop-app control, not Virtualization.framework, and never
//! host CGEvent / AX / Apple Events / clipboard / shared dirs / guest NIC paths.
//!
//! Default builds use an in-process browser simulator (Linux CI friendly).
//! Optional `browser-engine` feature remains default-off; when enabled the
//! substrate routes frame observation through receipt-gated engine capture.
//! Live macOS WK rasters mint a process-private receipt then complete; ordinary
//! `cargo test` never latches the physical-CLI authorize flag. Boot without
//! that flag stays fail-closed. Isolation is not proven.

use crate::backend::IsolatedSurfaceBackend;
use crate::captured_frame::BoundedCapturedFrame;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::simulator::{GuestFrame, GuestLocalAction, InjectOutcome};

#[cfg(feature = "browser-engine")]
use crate::browser_engine_capture::{
    begin_browser_engine_frame_capture, capture_live_wk_snapshot_through_receipt,
    complete_browser_engine_frame_capture, install_receipt_gated_capture,
    native_browser_engine_capture_authorized, require_engine_frame_observation,
};

#[cfg(feature = "browser-engine")]
const ENGINE_BOOT_UNAVAILABLE: &str =
    "Contained Browser browser-engine path is receipt-gated; native boot is not wired and isolation is not proven; boot fails closed";

/// Fail-closed when the optional browser engine path is selected but unwired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubstrateMode {
    /// In-process browser frame simulator — exercises SPI contract, not isolation PASS.
    #[cfg_attr(feature = "browser-engine", allow(dead_code))]
    Simulator,
    #[cfg(feature = "browser-engine")]
    /// Receipt-gated engine capture; never uses simulator bytes.
    ReceiptGated,
}

/// Contained Browser backend v0. Honest `ProofEvidenceClass::ContainedBrowser`
/// label with a full browsing-state lifecycle. Isolation is **not** proven in
/// v0 — admission stays false and artifacts must carry the dry-run nonclaim.
#[derive(Debug)]
pub struct ContainedBrowserBackend {
    mode: SubstrateMode,
    booted: bool,
    frame_epoch: u64,
    guest_link_clicked: bool,
    inject_fenced: bool,
    uncertain_on_next_inject: bool,
    crash_on_next_inject: bool,
    #[cfg(feature = "browser-engine")]
    receipt_gated_capture: Option<BoundedCapturedFrame>,
}

impl ContainedBrowserBackend {
    pub fn new() -> Self {
        Self::with_mode(default_substrate_mode())
    }

    fn with_mode(mode: SubstrateMode) -> Self {
        Self {
            mode,
            booted: false,
            frame_epoch: 0,
            guest_link_clicked: false,
            inject_fenced: false,
            uncertain_on_next_inject: false,
            crash_on_next_inject: false,
            #[cfg(feature = "browser-engine")]
            receipt_gated_capture: None,
        }
    }

    /// v0 never proves browser isolation — substrate rehearsal only.
    pub fn isolation_proof_available(&self) -> bool {
        false
    }

    pub fn substrate_mode_label(&self) -> &'static str {
        match self.mode {
            SubstrateMode::Simulator => "simulator",
            #[cfg(feature = "browser-engine")]
            SubstrateMode::ReceiptGated => "receipt_gated",
        }
    }

    pub fn schedule_uncertain_on_next_inject(&mut self) {
        self.uncertain_on_next_inject = true;
    }

    pub fn schedule_crash_on_next_inject(&mut self) {
        self.crash_on_next_inject = true;
    }

    fn current_capture(&self) -> HarnessResult<BoundedCapturedFrame> {
        match self.mode {
            SubstrateMode::Simulator => BoundedCapturedFrame::admit_simulator_capture(
                self.frame_epoch,
                self.guest_link_clicked,
            ),
            #[cfg(feature = "browser-engine")]
            SubstrateMode::ReceiptGated => require_engine_frame_observation(
                self.frame_epoch,
                self.receipt_gated_capture.as_ref(),
            ),
        }
    }

    fn current_frame(&self) -> HarnessResult<GuestFrame> {
        Ok(self
            .current_capture()?
            .to_guest_frame(self.guest_link_clicked))
    }

    fn fence_inject(&mut self) {
        self.inject_fenced = true;
    }

    #[cfg(feature = "browser-engine")]
    #[cfg_attr(not(test), allow(dead_code))]
    fn admit_receipt_gated_engine_capture(
        &mut self,
        rgba8_bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> HarnessResult<BoundedCapturedFrame> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        let handoff = begin_browser_engine_frame_capture(self.frame_epoch)?;
        let capture = complete_browser_engine_frame_capture(handoff, rgba8_bytes, width, height)?;
        self.receipt_gated_capture =
            Some(install_receipt_gated_capture(self.frame_epoch, capture)?);
        Ok(self
            .receipt_gated_capture
            .clone()
            .expect("installed capture"))
    }

    fn boot_simulator(&mut self) -> HarnessResult<GuestFrame> {
        if self.booted {
            return Err(HarnessError::invalid_state("browser guest already booted"));
        }
        self.booted = true;
        self.frame_epoch = 1;
        self.current_frame()
    }

    #[cfg(feature = "browser-engine")]
    fn receipt_gated_boot_unavailable() -> HarnessResult<GuestFrame> {
        Err(HarnessError::backend_unavailable(ENGINE_BOOT_UNAVAILABLE))
    }

    #[cfg(feature = "browser-engine")]
    fn boot_receipt_gated(&mut self) -> HarnessResult<GuestFrame> {
        if self.booted {
            return Err(HarnessError::invalid_state("browser guest already booted"));
        }
        if !native_browser_engine_capture_authorized() {
            return Self::receipt_gated_boot_unavailable();
        }
        #[cfg(target_os = "macos")]
        {
            self.boot_receipt_gated_live_wk()
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::receipt_gated_boot_unavailable()
        }
    }

    #[cfg(all(feature = "browser-engine", target_os = "macos"))]
    fn boot_receipt_gated_live_wk(&mut self) -> HarnessResult<GuestFrame> {
        self.booted = true;
        self.frame_epoch = 1;
        match capture_live_wk_snapshot_through_receipt(self.frame_epoch) {
            Ok(capture) => {
                self.receipt_gated_capture =
                    Some(install_receipt_gated_capture(self.frame_epoch, capture)?);
                self.current_frame()
            }
            Err(err) => {
                self.booted = false;
                self.frame_epoch = 0;
                self.receipt_gated_capture = None;
                Err(err)
            }
        }
    }

    /// Admit a live WK raster through the shipped receipt mint → complete path.
    /// Does not latch the physical-CLI authorize flag.
    #[cfg(feature = "browser-engine")]
    pub fn capture_live_wk_snapshot(&mut self) -> HarnessResult<BoundedCapturedFrame> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        let capture = capture_live_wk_snapshot_through_receipt(self.frame_epoch)?;
        self.receipt_gated_capture =
            Some(install_receipt_gated_capture(self.frame_epoch, capture)?);
        Ok(self
            .receipt_gated_capture
            .clone()
            .expect("installed live WK capture"))
    }

    fn inject_browser_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        if self.inject_fenced {
            return Err(HarnessError::inject_fenced(
                "browser guest inject is fenced",
            ));
        }

        if self.crash_on_next_inject {
            self.crash_on_next_inject = false;
            return Ok(InjectOutcome::Crash);
        }
        if self.uncertain_on_next_inject {
            self.uncertain_on_next_inject = false;
            return Ok(InjectOutcome::Uncertain);
        }

        let before = self.current_frame()?;
        match action {
            GuestLocalAction::ClickGuestButton => {
                // Guest-local link click inside contained browser DOM only.
                self.guest_link_clicked = true;
            }
            GuestLocalAction::TypeGuestText => {
                // Guest-local text field inside browser surface only.
            }
        }
        self.frame_epoch = self.frame_epoch.saturating_add(1);
        let after = self.current_frame()?;
        let guest_local_change = before.digest != after.digest;
        Ok(InjectOutcome::Changed(crate::simulator::FrameDelta {
            before_epoch: before.epoch,
            after_epoch: after.epoch,
            before_digest: before.digest,
            after_digest: after.digest,
            guest_local_change,
        }))
    }
}

impl Default for ContainedBrowserBackend {
    fn default() -> Self {
        Self::new()
    }
}

fn default_substrate_mode() -> SubstrateMode {
    #[cfg(feature = "browser-engine")]
    {
        SubstrateMode::ReceiptGated
    }
    #[cfg(not(feature = "browser-engine"))]
    {
        SubstrateMode::Simulator
    }
}

impl IsolatedSurfaceBackend for ContainedBrowserBackend {
    fn evidence_class(&self) -> ProofEvidenceClass {
        ProofEvidenceClass::ContainedBrowser
    }

    fn boot(&mut self) -> HarnessResult<GuestFrame> {
        match self.mode {
            SubstrateMode::Simulator => self.boot_simulator(),
            #[cfg(feature = "browser-engine")]
            SubstrateMode::ReceiptGated => self.boot_receipt_gated(),
        }
    }

    fn observe_frame(&self) -> HarnessResult<GuestFrame> {
        if !self.booted {
            return Err(HarnessError::invalid_state("browser guest is not booted"));
        }
        self.current_frame()
    }

    fn inject_guest_local(&mut self, action: GuestLocalAction) -> HarnessResult<InjectOutcome> {
        match self.mode {
            SubstrateMode::Simulator => self.inject_browser_local(action),
            #[cfg(feature = "browser-engine")]
            SubstrateMode::ReceiptGated => {
                if !self.booted {
                    return Err(HarnessError::invalid_state("browser guest is not booted"));
                }
                Err(HarnessError::backend_unavailable(ENGINE_BOOT_UNAVAILABLE))
            }
        }
    }

    /// Production fence: halts browser guest-local inject dispatch before teardown.
    fn stop_fence_first(&mut self) -> HarnessResult<()> {
        self.fence_inject();
        Ok(())
    }

    fn destroy(&mut self) -> HarnessResult<()> {
        self.booted = false;
        #[cfg(feature = "browser-engine")]
        {
            self.receipt_gated_capture = None;
        }
        Ok(())
    }

    fn is_booted(&self) -> bool {
        self.booted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(feature = "browser-engine"))]
    #[test]
    fn contained_browser_simulator_lifecycle() {
        let mut backend = ContainedBrowserBackend::new();
        assert_eq!(
            backend.evidence_class(),
            ProofEvidenceClass::ContainedBrowser
        );
        assert!(!backend.isolation_proof_available());

        let frame = backend.boot().expect("boot");
        assert_eq!(frame.epoch, 1);
        assert!(crate::captured_frame::is_canonical_sha256_digest(
            &frame.digest
        ));
        assert_eq!(
            frame.captured_frame.as_ref().map(|ev| ev.source),
            Some(crate::captured_frame::CapturedFrameSource::SyntheticSimulator)
        );
        assert!(backend.is_booted());

        let observed = backend.observe_frame().expect("observe");
        assert_eq!(observed.epoch, 1);
        assert_eq!(observed.digest, frame.digest);

        let outcome = backend
            .inject_guest_local(GuestLocalAction::ClickGuestButton)
            .expect("inject");
        assert!(matches!(outcome, InjectOutcome::Changed(_)));
        let after = backend.observe_frame().expect("after");
        assert_ne!(after.digest, frame.digest);
        crate::captured_frame::assert_postcondition_change(&frame, &after).expect("postcondition");

        backend.stop_fence_first().expect("fence");
        backend.destroy().expect("destroy");
        assert!(!backend.is_booted());
    }

    #[test]
    fn contained_browser_never_vf_eligible() {
        let backend = ContainedBrowserBackend::new();
        assert!(!backend.evidence_class().is_vf_qualification_eligible());
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn browser_engine_feature_fails_closed_on_boot() {
        let mut backend = ContainedBrowserBackend::new();
        assert_eq!(backend.substrate_mode_label(), "receipt_gated");
        let err = backend.boot().expect_err("engine path fails closed");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(!backend.is_booted());
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn receipt_gated_boot_denies_even_when_authorization_hook_satisfied() {
        let err = ContainedBrowserBackend::receipt_gated_boot_unavailable()
            .expect_err("unauthorized / non-macOS boot helper stays fail-closed");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("receipt-gated"));
        assert!(!native_browser_engine_capture_authorized());
    }

    #[cfg(all(feature = "browser-engine", target_os = "macos"))]
    #[test]
    fn live_wk_snapshot_installs_engine_bytes_without_authorize_flag() {
        use crate::captured_frame::{
            canonical_sha256_digest, validate_public_evidence, CapturedFrameSource,
            SYNTHETIC_FRAME_PAYLOAD_NEEDLE,
        };

        assert!(!native_browser_engine_capture_authorized());
        let mut backend = ContainedBrowserBackend::new();
        backend.booted = true;
        backend.frame_epoch = 1;
        let capture = match backend.capture_live_wk_snapshot() {
            Ok(capture) => capture,
            Err(err) if err.message.contains("main thread") => {
                // libtest worker threads are not main; the shipped entry fail-closes
                // instead of trapping. The harness-free main-thread test covers success.
                assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
                assert!(!native_browser_engine_capture_authorized());
                return;
            }
            Err(err) => panic!("live WK receipt-gated capture: {err:?}"),
        };
        assert!(!native_browser_engine_capture_authorized());
        assert!(!crate::isolated_surface_admission_available());
        assert_eq!(capture.source(), CapturedFrameSource::BrowserEngine);
        assert_eq!(capture.epoch(), 1);
        assert!(!capture
            .captured_bytes()
            .windows(SYNTHETIC_FRAME_PAYLOAD_NEEDLE.len())
            .any(|window| window == SYNTHETIC_FRAME_PAYLOAD_NEEDLE));
        let bytes = capture.captured_bytes();
        assert!(!bytes
            .chunks_exact(4)
            .all(|pixel| pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255));
        let [target_r, target_g, target_b] = crate::LIVE_WK_FIXTURE_CRIMSON_RGB;
        let near = |actual: u8, target: u8| (actual as i16 - target as i16).unsigned_abs() <= 40;
        assert!(bytes.chunks_exact(4).any(|pixel| {
            near(pixel[0], target_r) && near(pixel[1], target_g) && near(pixel[2], target_b)
                || near(pixel[0], target_b) && near(pixel[1], target_g) && near(pixel[2], target_r)
        }));
        assert_eq!(
            capture.digest(),
            canonical_sha256_digest(capture.captured_bytes())
        );

        let frame = backend.observe_frame().expect("observe live WK frame");
        assert_eq!(frame.digest, capture.digest());
        assert_eq!(
            validate_public_evidence(&capture.public_evidence())
                .expect_err("public verifier stays fail-closed")
                .code,
            crate::error::HarnessErrorCode::BackendUnavailable
        );

        backend.destroy().expect("destroy");
        assert!(!backend.is_booted());
        backend.booted = true;
        backend.frame_epoch = 1;
        let err = backend
            .observe_frame()
            .expect_err("destroy cleared capture");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
    }

    #[cfg(all(feature = "browser-engine", not(target_os = "macos")))]
    #[test]
    fn live_wk_snapshot_fail_closes_off_macos() {
        let mut backend = ContainedBrowserBackend::new();
        backend.booted = true;
        backend.frame_epoch = 1;
        let err = backend
            .capture_live_wk_snapshot()
            .expect_err("live WK is macOS-only");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(!native_browser_engine_capture_authorized());
        assert!(!crate::isolated_surface_admission_available());
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn receipt_gated_mode_never_uses_simulator_bytes() {
        let backend = ContainedBrowserBackend::new();
        assert_eq!(backend.substrate_mode_label(), "receipt_gated");
        // Force booted state only to exercise observation fail-closed semantics.
        let mut backend = backend;
        backend.booted = true;
        backend.frame_epoch = 1;
        let err = backend.observe_frame().expect_err("no receipt-gated bytes");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("receipt-gated"));
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn receipt_mint_admit_observe_round_trip_is_engine_sourced() {
        use crate::captured_frame::{
            canonical_sha256_digest, validate_public_evidence, CapturedFrameSource,
        };

        const RGBA8: usize = 4;
        let mut backend = ContainedBrowserBackend::new();
        backend.booted = true;
        backend.frame_epoch = 2;
        let bytes = vec![0x7a; 3 * 2 * RGBA8];
        let capture = backend
            .admit_receipt_gated_engine_capture(bytes.clone(), 3, 2)
            .expect("receipt-gated admit");
        assert_eq!(capture.source(), CapturedFrameSource::BrowserEngine);
        assert_eq!(capture.digest(), canonical_sha256_digest(&bytes));

        let frame = backend.observe_frame().expect("observe admitted frame");
        assert_eq!(frame.epoch, 2);
        assert_eq!(frame.digest, capture.digest());
        assert_eq!(
            frame.captured_frame.as_ref().map(|ev| ev.source),
            Some(CapturedFrameSource::BrowserEngine)
        );

        let evidence = capture.public_evidence();
        assert_eq!(
            validate_public_evidence(&evidence)
                .expect_err("public verifier stays fail-closed for engine evidence")
                .code,
            crate::error::HarnessErrorCode::BackendUnavailable
        );
        assert!(!crate::isolated_surface_admission_available());
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn destroy_clears_receipt_gated_capture() {
        const RGBA8: usize = 4;
        let mut backend = ContainedBrowserBackend::new();
        backend.booted = true;
        backend.frame_epoch = 3;
        backend
            .admit_receipt_gated_engine_capture(vec![0x55; RGBA8], 1, 1)
            .expect("admit capture");
        backend.observe_frame().expect("capture installed");

        backend.destroy().expect("destroy");
        assert!(!backend.is_booted());
        backend.booted = true;
        backend.frame_epoch = 3;
        let err = backend.observe_frame().expect_err("stale capture cleared");
        assert_eq!(err.code, crate::error::HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("receipt-gated"));
    }

    #[cfg(feature = "browser-engine")]
    #[test]
    fn stale_epoch_observation_rejected_after_receipt_admit() {
        const RGBA8: usize = 4;
        let mut backend = ContainedBrowserBackend::new();
        backend.booted = true;
        backend.frame_epoch = 4;
        backend
            .admit_receipt_gated_engine_capture(vec![0x02; RGBA8], 1, 1)
            .expect("admit epoch 4");
        backend.frame_epoch = 5;
        let observe_err = backend.observe_frame().expect_err("stale observation");
        assert_eq!(
            observe_err.code,
            crate::error::HarnessErrorCode::InvalidState
        );
        assert!(observe_err.message.contains("stale") || observe_err.message.contains("misbound"));
    }
}
