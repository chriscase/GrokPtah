//! Exclusive ownership of the host home.
//!
//! ADR-002 makes a second concurrent writer to a GrokPtah home a boundary
//! change, not an implementation detail. The host therefore owns its home
//! exclusively for as long as it runs: a second host, or an operator command
//! issued while a host is serving, is refused rather than allowed to interleave
//! writes.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{HostError, HostResult, io_error};

/// File name of the exclusive home lock.
pub const LOCK_FILE_NAME: &str = "headless-host.lock";

/// A held exclusive lock on the host home. Released on drop.
#[derive(Debug)]
pub struct HomeLock {
    file: File,
    path: PathBuf,
}

impl HomeLock {
    /// Take the exclusive home lock, or fail closed if another writer holds it.
    pub fn acquire(home: &Path, owner_note: &str) -> HostResult<Self> {
        std::fs::create_dir_all(home).map_err(|error| io_error("home_unwritable", &error))?;
        let path = home.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| io_error("lock_unopenable", &error))?;

        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                return Err(HostError::unavailable(
                    "home_locked",
                    "another host already owns this home",
                ));
            }
            Err(TryLockError::Error(error)) => {
                return Err(io_error("lock_unavailable", &error));
            }
        }

        let mut lock = Self { file, path };
        lock.write_owner_note(owner_note)?;
        Ok(lock)
    }

    /// Whether a home is currently owned by some host.
    ///
    /// This is a probe: it takes and immediately releases the lock, so it can
    /// race with a host that is starting. Health reports it as observed state,
    /// never as authorization.
    pub fn is_held(home: &Path) -> bool {
        let path = home.join(LOCK_FILE_NAME);
        let Ok(file) = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
        else {
            return false;
        };
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                false
            }
            Err(TryLockError::WouldBlock) => true,
            Err(TryLockError::Error(_)) => false,
        }
    }

    /// Path of the lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn write_owner_note(&mut self, owner_note: &str) -> HostResult<()> {
        self.file
            .set_len(0)
            .map_err(|error| io_error("lock_unwritable", &error))?;
        writeln!(self.file, "{owner_note}").map_err(|error| io_error("lock_unwritable", &error))?;
        self.file
            .sync_data()
            .map_err(|error| io_error("lock_unwritable", &error))
    }
}

impl Drop for HomeLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_host_is_refused_and_the_first_release_frees_the_home() {
        let home = tempfile::tempdir().expect("temp home");
        assert!(!HomeLock::is_held(home.path()));

        let first = HomeLock::acquire(home.path(), "pid=1").expect("first lock");
        assert!(HomeLock::is_held(home.path()));

        let second = HomeLock::acquire(home.path(), "pid=2");
        let error = second.expect_err("second host must be refused");
        assert_eq!(error.reason_code(), "home_locked");
        assert_eq!(
            error.envelope().code,
            grokptah_agent_sdk::ErrorCode::AuthorityUnavailable
        );

        assert!(first.path().ends_with(LOCK_FILE_NAME));
        drop(first);
        assert!(!HomeLock::is_held(home.path()));
        HomeLock::acquire(home.path(), "pid=3").expect("home is free again");
    }
}
