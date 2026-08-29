//! Crash cuts: named points at which a test may interrupt the send path (#478).
//!
//! Durability claims are only worth what the crash tests prove, so every point
//! where the on-disk state could disagree with reality gets a name here and a
//! checkpoint call at the exact place it describes.
//!
//! Arming is process-global and deliberately public: the two-process restart and
//! process-kill tests need to arm a cut from a spawned binary, which cannot see
//! `cfg(test)`. The structural gate (`tests/provider_send_gate.rs`) enforces that
//! no production path in `src/` ever arms one.

use std::sync::atomic::{AtomicU8, Ordering};

/// A named interruption point on the physical send path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CrashCut {
    /// Before any durable intent exists.
    BeforeIntent = 1,
    /// After `Preparing` is durable, before admission completes.
    AfterPreparing = 2,
    /// After `Sending` is durable, before the send future is created.
    AfterSendingBeforeBytes = 3,
    /// While request bytes are being written.
    MidWrite = 4,
    /// After request bytes were written, before any response header arrived.
    AfterBytesNoHeaders = 5,
    /// After response headers, before any body byte.
    AfterHeaders = 6,
    /// After a non-streaming body was read.
    AfterBody = 7,
    /// Part-way through a streaming response.
    MidStream = 8,
    /// After the response is complete, before the settlement bundle is written.
    /// Proves the bundle cannot half-land as "settled but no receipt".
    SettlementBeforeReceipt = 9,
    /// After the response is complete, before the settlement bundle is written.
    /// Proves the bundle cannot half-land as "settled but no audit outcome".
    SettlementBeforeAudit = 10,
}

impl CrashCut {
    pub const ALL: [Self; 10] = [
        Self::BeforeIntent,
        Self::AfterPreparing,
        Self::AfterSendingBeforeBytes,
        Self::MidWrite,
        Self::AfterBytesNoHeaders,
        Self::AfterHeaders,
        Self::AfterBody,
        Self::MidStream,
        Self::SettlementBeforeReceipt,
        Self::SettlementBeforeAudit,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::BeforeIntent => "before_intent",
            Self::AfterPreparing => "after_preparing",
            Self::AfterSendingBeforeBytes => "after_sending_before_bytes",
            Self::MidWrite => "mid_write",
            Self::AfterBytesNoHeaders => "after_bytes_no_headers",
            Self::AfterHeaders => "after_headers",
            Self::AfterBody => "after_body",
            Self::MidStream => "mid_stream",
            Self::SettlementBeforeReceipt => "settlement_before_receipt",
            Self::SettlementBeforeAudit => "settlement_before_audit",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|cut| cut.as_str() == value)
    }
}

/// What happens when an armed cut fires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum CutAction {
    /// Kill the process without unwinding, the way a real crash does.
    Abort = 1,
    /// Return an interruption error, so an in-process test can then exercise
    /// recovery against the same durable state a real crash would have left.
    Interrupt = 2,
}

static ARMED_CUT: AtomicU8 = AtomicU8::new(0);
static ARMED_ACTION: AtomicU8 = AtomicU8::new(0);
static CUT_SERIALIZER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serializes tests that arm a cut. Arming is process-global, so two tests
/// arming at once would read each other's cut. Held for the whole of a cut
/// test, released when the guard drops.
pub fn crash_cut_test_lock() -> std::sync::MutexGuard<'static, ()> {
    CUT_SERIALIZER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Arm one crash cut for this process.
///
/// Never called from production code; the structural gate enforces that.
pub fn arm_crash_cut(cut: CrashCut, action: CutAction) {
    ARMED_ACTION.store(action as u8, Ordering::SeqCst);
    ARMED_CUT.store(cut as u8, Ordering::SeqCst);
}

/// Disarm any armed cut.
pub fn disarm_crash_cut() {
    ARMED_CUT.store(0, Ordering::SeqCst);
    ARMED_ACTION.store(0, Ordering::SeqCst);
}

/// Raised when an armed `Interrupt` cut fires.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CutFired(pub CrashCut);

impl std::fmt::Display for CutFired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "crash cut fired at {}", self.0.as_str())
    }
}

impl std::error::Error for CutFired {}

/// Evaluate a cut point. Fires at most once: the cut disarms itself so a retry
/// loop does not spin on the same interruption.
pub(crate) fn checkpoint(cut: CrashCut) -> Result<(), CutFired> {
    if ARMED_CUT.load(Ordering::SeqCst) != cut as u8 {
        return Ok(());
    }
    let action = ARMED_ACTION.load(Ordering::SeqCst);
    disarm_crash_cut();
    if action == CutAction::Abort as u8 {
        // No unwinding, no destructors, no flush: exactly what a kill does.
        std::process::abort();
    }
    Err(CutFired(cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_names_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for cut in CrashCut::ALL {
            assert!(seen.insert(cut.as_str()), "duplicate cut name");
            assert_eq!(CrashCut::parse(cut.as_str()), Some(cut));
        }
        assert_eq!(CrashCut::parse("nonexistent"), None);
    }

    #[test]
    fn an_unarmed_checkpoint_is_a_no_op() {
        let _guard = crash_cut_test_lock();
        disarm_crash_cut();
        for cut in CrashCut::ALL {
            assert!(checkpoint(cut).is_ok());
        }
    }

    #[test]
    fn an_armed_interrupt_fires_once() {
        let _guard = crash_cut_test_lock();
        arm_crash_cut(CrashCut::AfterPreparing, CutAction::Interrupt);
        assert_eq!(
            checkpoint(CrashCut::AfterPreparing),
            Err(CutFired(CrashCut::AfterPreparing))
        );
        assert!(checkpoint(CrashCut::AfterPreparing).is_ok());
    }

    #[test]
    fn an_armed_cut_does_not_fire_at_a_different_point() {
        let _guard = crash_cut_test_lock();
        arm_crash_cut(CrashCut::MidStream, CutAction::Interrupt);
        assert!(checkpoint(CrashCut::AfterHeaders).is_ok());
        assert_eq!(
            checkpoint(CrashCut::MidStream),
            Err(CutFired(CrashCut::MidStream))
        );
        disarm_crash_cut();
    }
}
