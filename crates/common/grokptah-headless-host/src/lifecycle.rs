//! Process lifecycle and shutdown escalation.
//!
//! Shutdown is a monotonic ladder: the first request drains, a second request
//! stops immediately. It never de-escalates, so a late graceful request cannot
//! cancel an immediate stop already in flight.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

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
    fn clones_share_one_state() {
        let signal = ShutdownSignal::new();
        let watcher = signal.clone();
        watcher.request(ShutdownKind::Graceful);
        assert_eq!(signal.state(), ShutdownKind::Graceful);
        assert_eq!(ShutdownKind::Graceful.label(), "graceful");
    }
}
