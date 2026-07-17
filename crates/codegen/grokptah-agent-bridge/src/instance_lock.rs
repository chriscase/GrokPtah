//! Single-instance advisory lock over `~/.grokptah` (or GROKPTAH_HOME).
//!
//! Prevents two desktop processes from double-appending transcripts and
//! racing GC (#119).

use std::fs::{File, OpenOptions};
use std::io::Write;

use anyhow::{Context, Result};
use fs2::FileExt;

use crate::discover::{ensure_home, grokptah_home};

/// Held for the lifetime of the agent host. Drop releases the exclusive lock.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Try to acquire an exclusive non-blocking lock on `~/.grokptah/.instance.lock`.
    ///
    /// Returns an error if another live process already holds the lock.
    pub fn try_acquire() -> Result<Self> {
        ensure_home();
        let path = grokptah_home().join(".instance.lock");
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("open instance lock {}", path.display()))?;

        file.try_lock_exclusive().map_err(|e| {
            anyhow::anyhow!(
                "another GrokPtah instance is already using {} ({e}). \
                 Quit the other app (or stale build) before starting a second one.",
                grokptah_home().display()
            )
        })?;

        // Best-effort pid stamp for operators debugging locks.
        let _ = file.set_len(0);
        let _ = writeln!(
            file,
            "pid={} home={}",
            std::process::id(),
            grokptah_home().display()
        );
        let _ = file.sync_all();

        Ok(Self { _file: file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::{home_override_serial, set_grokptah_home_override};

    #[test]
    fn second_lock_fails_while_first_held() {
        let _g = home_override_serial();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(&home).unwrap();
        set_grokptah_home_override(Some(home));
        let first = InstanceLock::try_acquire().expect("first lock");
        let second = InstanceLock::try_acquire();
        assert!(second.is_err(), "second instance must be refused");
        drop(first);
        let third = InstanceLock::try_acquire().expect("lock after drop");
        drop(third);
        set_grokptah_home_override(None);
    }
}
