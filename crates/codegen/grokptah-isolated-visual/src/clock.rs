use std::sync::Mutex;

use chrono::{DateTime, Duration, Utc};

pub trait HostClock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl HostClock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Host-owned test clock. Lease expiry and crash-cut tests must not read the
/// wall clock; jump/rollback is explicit.
#[derive(Debug)]
pub struct TestClock {
    now: Mutex<DateTime<Utc>>,
}

impl TestClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self.now.lock().expect("test clock") = now;
    }

    pub fn jump(&self, delta: Duration) {
        let mut now = self.now.lock().expect("test clock");
        *now += delta;
    }
}

impl HostClock for TestClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().expect("test clock")
    }
}

impl HostClock for std::sync::Arc<TestClock> {
    fn now(&self) -> DateTime<Utc> {
        TestClock::now(self)
    }
}
