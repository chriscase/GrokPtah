use std::ffi::{OsStr, OsString};
use std::sync::MutexGuard;

use grokptah_agent_bridge::{home_override_serial, set_grokptah_home_override};

/// Serializes and restores process-global environment mutations in a test
/// binary. Each integration test binary has its own process environment, so
/// the existing home mutex is the correct synchronization boundary here.
pub struct ProcessEnvGuard {
    _lock: MutexGuard<'static, ()>,
    previous: Vec<(OsString, Option<OsString>)>,
}

impl ProcessEnvGuard {
    pub fn new() -> Self {
        Self {
            _lock: home_override_serial(),
            previous: Vec::new(),
        }
    }

    pub fn set(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let key = key.as_ref();
        self.remember(key);
        // SAFETY: the guard serializes all test mutations within this process.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    #[allow(dead_code)]
    pub fn remove(&mut self, key: impl AsRef<OsStr>) {
        let key = key.as_ref();
        self.remember(key);
        // SAFETY: the guard serializes all test mutations within this process.
        unsafe {
            std::env::remove_var(key);
        }
    }

    fn remember(&mut self, key: &OsStr) {
        self.previous
            .push((key.to_os_string(), std::env::var_os(key)));
    }
}

impl Drop for ProcessEnvGuard {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        for (key, previous) in self.previous.drain(..).rev() {
            // SAFETY: the guard still owns the process-global test boundary.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}
