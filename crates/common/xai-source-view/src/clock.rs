//! Injected time.
//!
//! Token expiry, snapshot sweeping, and replay refusal are all time-dependent,
//! so time is a parameter rather than an ambient call. Tests drive a
//! [`TestClock`] and assert exact boundaries instead of sleeping.

use std::sync::atomic::{AtomicU64, Ordering};

/// Milliseconds since the Unix epoch.
pub trait Clock: Send + Sync + std::fmt::Debug {
    fn now_ms(&self) -> u64;
}

/// Wall clock. Saturates rather than panicking if the host clock predates the
/// epoch, so a misconfigured machine degrades to "everything is expired".
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|delta| u64::try_from(delta.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

/// Manually advanced clock for tests.
#[derive(Debug)]
pub struct TestClock(AtomicU64);

impl TestClock {
    pub fn new(start_ms: u64) -> Self {
        Self(AtomicU64::new(start_ms))
    }

    pub fn advance_ms(&self, delta: u64) {
        self.0.fetch_add(delta, Ordering::SeqCst);
    }

    pub fn set_ms(&self, value: u64) {
        self.0.store(value, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
