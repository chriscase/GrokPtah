//! Single-instance advisory lock over `~/.grokptah` (or GROKPTAH_HOME).
//!
//! Prevents two desktop processes from double-appending transcripts and
//! racing GC (#119).

use std::fs::{File, OpenOptions};
use std::io::Write;

use anyhow::{Context, Result};
use fs2::FileExt;

use crate::discover::{ensure_home, RuntimeHome};

/// Held for the lifetime of the agent host. Drop releases the exclusive lock.
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// Try to acquire an exclusive non-blocking lock on `~/.grokptah/.instance.lock`.
    ///
    /// Returns an error if another live process already holds the lock.
    #[allow(dead_code)]
    pub fn try_acquire() -> Result<Self> {
        ensure_home();
        Self::try_acquire_at(&RuntimeHome::discover())
    }

    /// Acquire the lock for an explicitly selected runtime home.
    ///
    /// Authority comes **before** layout. Only the one directory the lock file
    /// itself lives in may be created before ownership is proven; the rest of
    /// the home tree is prepared afterwards, under the lock. A contender that
    /// is going to be refused must not be able to lay down a home tree on a
    /// home it does not own (#455).
    pub fn try_acquire_at(home: &RuntimeHome) -> Result<Self> {
        let lock = Self::try_acquire_path(&home.instance_lock_path(), home.path())?;
        refuse_if_a_nested_store_is_owned(home)?;
        home.prepare()?;
        Ok(lock)
    }

    /// Acquire the lock directly at its path, creating only the directory the
    /// lock file itself needs.
    ///
    /// This is the minimal-footprint acquisition used by offline maintenance
    /// handles: they must own the home *before* laying down any store layout,
    /// so the only filesystem mutation that may precede authority is the
    /// directory the lock file lives in (#455).
    pub(crate) fn try_acquire_path(
        path: &std::path::Path,
        home_label: &std::path::Path,
    ) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create runtime home {}", parent.display()))?;
        }
        Self::open_and_lock(path, home_label)
    }

    /// Test seam: take a lock directly at a path, modelling an offline handle
    /// that resolved its own home.
    pub fn try_acquire_path_for_test(
        path: &std::path::Path,
        home_label: &std::path::Path,
    ) -> Result<Self> {
        Self::try_acquire_path(path, home_label)
    }

    fn open_and_lock(path: &std::path::Path, home_label: &std::path::Path) -> Result<Self> {
        // Deliberately **not** truncating. Truncation is a mutation of the live
        // owner's lock file, and doing it before `flock` means a contender that
        // is about to be refused has already erased the owner's pid stamp — the
        // one piece of evidence an operator uses to find the process holding
        // the home. The file is truncated below, after ownership is proven
        // (#455).
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!("open instance lock {}", path.display()))?;

        file.try_lock_exclusive().map_err(|e| {
            anyhow::anyhow!(
                "another GrokPtah instance is already using {} ({e}). \
                 Quit the other app (or stale build) before starting a second one.",
                home_label.display()
            )
        })?;

        // Owned now, so rewriting the stamp is this process's to do.
        let _ = file.set_len(0);
        use std::io::Seek;
        let _ = file.seek(std::io::SeekFrom::Start(0));
        let _ = writeln!(
            file,
            "pid={} home={}",
            std::process::id(),
            home_label.display()
        );
        let _ = file.sync_all();

        Ok(Self { _file: file })
    }
}

/// Refuse a home whose durable roots are already owned by an offline handle.
///
/// A durable root inside a home is normally governed by the home's own lock. A
/// root opened *before* any runtime could be identified governs itself instead,
/// taking `<root>/.instance.lock`. If a runtime then acquired the home lock, the
/// two would be different locks over overlapping state — two writers, each
/// correctly holding "its" lock.
///
/// Holding the home lock is therefore not sufficient: the home is only ours if
/// no nested root is separately owned. Probing is sound here because we already
/// hold the home lock, so no *new* nested owner can appear — a nested root can
/// only be taken by a handle that resolved its home before we did, and this runs
/// after our acquisition (#455).
fn refuse_if_a_nested_store_is_owned(home: &RuntimeHome) -> Result<()> {
    for root in [home.orchestration_root(), home.computer_root()] {
        let nested = root.join(".instance.lock");
        if instance_lock_is_held(&nested) {
            anyhow::bail!(
                "a durable store under {} is separately owned ({} is held), so this home \
                 cannot be taken without creating two writers over the same state. Stop the \
                 process or offline maintenance handle holding it first.",
                home.path().display(),
                nested.display()
            );
        }
    }
    Ok(())
}

/// Whether **any** process — including this one, through another file
/// description — currently holds the advisory lock for this home.
///
/// Read-only: it never truncates or rewrites the lock file, so probing cannot
/// clobber the live owner's pid stamp. `flock` is per open-file-description,
/// so a second descriptor in this process conflicts exactly as another
/// process's would, which is what makes this a usable ownership token rather
/// than a same-process no-op.
///
/// A `false` return means the home was unowned at the instant of the probe.
/// Callers must treat it as "no owner to defer to", never as authority to
/// hold across later writes.
pub fn instance_lock_is_held(lock_path: &std::path::Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(lock_path) else {
        // No lock file means no owner has ever prepared this home.
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(_) => true,
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
