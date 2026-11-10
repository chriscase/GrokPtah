//! Process lifecycle and shutdown escalation.
//!
//! Shutdown is a monotonic ladder: the first request drains, a second request
//! stops immediately. It never de-escalates, so a late graceful request cannot
//! cancel an immediate stop already in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// How the host was asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ShutdownKind {
    /// No stop has been requested.
    None,
    /// Finish the in-flight step, checkpoint, then stop.
    Graceful,
    /// Stop now; active runs recover on the next start.
    Immediate,
}

impl ShutdownKind {
    fn from_code(code: u8) -> Self {
        match code {
            0 => Self::None,
            1 => Self::Graceful,
            _ => Self::Immediate,
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Graceful => 1,
            Self::Immediate => 2,
        }
    }

    /// Stable label for logs and receipts.
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Graceful => "graceful",
            Self::Immediate => "immediate",
        }
    }
}

/// Shared, clonable shutdown state.
///
/// The value is a plain atomic so an OS signal watcher on another thread can
/// escalate it without taking a lock the serve loop also needs.
#[derive(Debug, Clone, Default)]
pub struct ShutdownSignal {
    state: Arc<AtomicU8>,
}

impl ShutdownSignal {
    /// A signal with no stop requested.
    pub fn new() -> Self {
        Self::default()
    }

    /// Escalate to `kind` and return the resulting state.
    pub fn request(&self, kind: ShutdownKind) -> ShutdownKind {
        let requested = kind.code();
        let mut current = self.state.load(Ordering::SeqCst);
        loop {
            if current >= requested {
                return ShutdownKind::from_code(current);
            }
            match self.state.compare_exchange(
                current,
                requested,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return kind,
                Err(observed) => current = observed,
            }
        }
    }

    /// Current stop state.
    pub fn state(&self) -> ShutdownKind {
        ShutdownKind::from_code(self.state.load(Ordering::SeqCst))
    }

    /// Whether any stop has been requested.
    pub fn is_requested(&self) -> bool {
        self.state() != ShutdownKind::None
    }
}

/// Cooperative cancellation for one in-flight engine step.
///
/// The host core is synchronous: while a step is running, the control loop is
/// inside it and cannot process another command. A long-running step therefore
/// needs a cancellation channel that does not depend on the loop making
/// progress, and this is it — a plain atomic another thread (the OS signal
/// watcher) can trip.
///
/// It is cooperative by construction. Nothing here interrupts a step that
/// declines to look; an engine that ignores the signal simply runs to
/// completion, which is why the host treats a step it could not stop as
/// finished rather than as cancelled.
///
/// A fresh signal is issued per dispatch. There is deliberately no reset: a
/// cancelled signal that could be revived would let a stale cancellation be
/// cleared by the very code it was meant to stop.
#[derive(Debug, Clone, Default)]
pub struct CancelSignal {
    cancelled: std::sync::Arc<AtomicBool>,
}

impl CancelSignal {
    /// A signal that has not been tripped.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the in-flight step to stop at its next checkpoint.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

/// Coarse lifecycle state reported by health.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostState {
    /// Configuration validated, store not yet open.
    Starting,
    /// Accepting operator commands.
    Ready,
    /// Stop requested; finishing in-flight work.
    Draining,
    /// Stopped and unlocked.
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escalation_is_monotonic_and_never_reverses() {
        let signal = ShutdownSignal::new();
        assert_eq!(signal.state(), ShutdownKind::None);
        assert!(!signal.is_requested());

        assert_eq!(
            signal.request(ShutdownKind::Graceful),
            ShutdownKind::Graceful
        );
        assert_eq!(
            signal.request(ShutdownKind::Immediate),
            ShutdownKind::Immediate
        );
        // A late graceful request cannot walk an immediate stop back.
        assert_eq!(
            signal.request(ShutdownKind::Graceful),
            ShutdownKind::Immediate
        );
        assert_eq!(signal.state(), ShutdownKind::Immediate);
    }

    #[test]
    fn a_cancel_signal_trips_once_and_stays_tripped() {
        let signal = CancelSignal::new();
        let watcher = signal.clone();
        assert!(!signal.is_cancelled());
        watcher.cancel();
        assert!(signal.is_cancelled());
        // Idempotent: a second request changes nothing.
        watcher.cancel();
        assert!(signal.is_cancelled());
    }

    #[test]
    fn clones_share_one_state() {
        let signal = ShutdownSignal::new();
        let watcher = signal.clone();
        watcher.request(ShutdownKind::Graceful);
        assert_eq!(signal.state(), ShutdownKind::Graceful);
        assert_eq!(ShutdownKind::Graceful.label(), "graceful");
    }
}
