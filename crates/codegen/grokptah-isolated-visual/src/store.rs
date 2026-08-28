//! Durable, single-writer store for guests and surface leases.
//!
//! Three properties matter here and each is load-bearing:
//!
//! * **Exclusive.** One process at a time holds an advisory lock on the root.
//!   A second opener is refused rather than racing.
//! * **Atomic and durable.** Records are written to a temp file, fsynced,
//!   renamed, and the directory is fsynced. A caller that gets `Ok` may rely on
//!   the record surviving a crash; a caller that gets `Err` must treat the
//!   write as not having happened.
//! * **Quarantining.** A record that deserializes but does not satisfy its own
//!   `validate()` is *not* a usable record. Deserialization succeeding says
//!   only that the bytes had the right shape. Such records are moved to
//!   `quarantine/` so recovery neither trusts them nor wedges on them.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::Serialize;

use crate::error::{IsolatedError, IsolatedErrorCode, IsolatedResult};
use crate::ids::safe_file_id;
use crate::lease::{ComputerDispatchState, ComputerSurfaceLease, ComputerSurfaceLeaseState};
use crate::lifecycle::{IsolatedGuestRecord, IsolatedGuestTerminal};

const MAX_RECORD_BYTES: u64 = 1024 * 1024;

/// What recovery did to the store, so callers can report it rather than
/// discovering it later.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Files moved aside because they were unreadable, oversized, or invalid.
    pub quarantined: Vec<String>,
    /// Leases whose grant had expired and were reaped on open.
    pub expired_reaped: Vec<String>,
    /// Leases carried from Injected to Uncertain across a restart.
    pub uncertain_after_restart: Vec<String>,
}

pub struct IsolatedVisualStore {
    inner: Arc<IsolatedVisualStoreInner>,
    recovery: RecoveryReport,
}

struct IsolatedVisualStoreInner {
    root: PathBuf,
    _store_lock: fs::File,
}

impl IsolatedVisualStore {
    pub fn open(root: impl AsRef<Path>, now: DateTime<Utc>) -> IsolatedResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("guests")).map_err(io_err)?;
        fs::create_dir_all(root.join("leases")).map_err(io_err)?;
        fs::create_dir_all(root.join("quarantine")).map_err(io_err)?;
        let root = dunce::canonicalize(&root).map_err(io_err)?;
        let lock_path = root.join(".store.lock");
        let store_lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        store_lock.try_lock_exclusive().map_err(|error| {
            IsolatedError::conflict(format!(
                "isolated visual store {} is already open ({error})",
                root.display()
            ))
        })?;
        let mut store = Self {
            inner: Arc::new(IsolatedVisualStoreInner {
                root,
                _store_lock: store_lock,
            }),
            recovery: RecoveryReport::default(),
        };
        let report = store.recover(now)?;
        store.recovery = report;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// What the last `open` had to repair. Surfaced rather than silent.
    pub fn recovery(&self) -> &RecoveryReport {
        &self.recovery
    }

    pub fn save_guest(&self, guest: &IsolatedGuestRecord) -> IsolatedResult<()> {
        guest.validate()?;
        atomic_write_json(&self.guest_path(&guest.guest_id)?, guest)
    }

    pub fn load_guest(&self, guest_id: &str) -> IsolatedResult<Option<IsolatedGuestRecord>> {
        let path = self.guest_path(guest_id)?;
        match read_optional::<IsolatedGuestRecord>(&path)? {
            None => Ok(None),
            Some(guest) => {
                guest.validate()?;
                Ok(Some(guest))
            }
        }
    }

    pub fn list_guests(&self) -> IsolatedResult<Vec<IsolatedGuestRecord>> {
        read_all(&self.inner.root.join("guests"))
    }

    pub fn save_lease(&self, lease: &ComputerSurfaceLease) -> IsolatedResult<()> {
        lease.validate()?;
        atomic_write_json(&self.lease_path(&lease.lease_id)?, lease)
    }

    pub fn load_lease(&self, lease_id: &str) -> IsolatedResult<Option<ComputerSurfaceLease>> {
        let path = self.lease_path(lease_id)?;
        match read_optional::<ComputerSurfaceLease>(&path)? {
            None => Ok(None),
            Some(lease) => {
                lease.validate()?;
                Ok(Some(lease))
            }
        }
    }

    pub fn list_leases(&self) -> IsolatedResult<Vec<ComputerSurfaceLease>> {
        read_all(&self.inner.root.join("leases"))
    }

    /// Reap leases whose grant window has passed. An expired lease is not a
    /// live grant, so it is transitioned to `Revoked` rather than left where a
    /// later dispatch could find it in `Granted`.
    pub fn reap_expired(&self, now: DateTime<Utc>) -> IsolatedResult<Vec<String>> {
        let mut reaped = Vec::new();
        for mut lease in self.list_leases()? {
            if lease.state.is_terminal() || now < lease.expires_at {
                continue;
            }
            match lease.dispatch.as_mut() {
                Some(dispatch) if dispatch.state == ComputerDispatchState::Injected => {
                    // Expiry cannot un-inject. The physical outcome stays
                    // uncertain and is never replayed.
                    dispatch.state = ComputerDispatchState::Uncertain;
                    dispatch.completed_at = Some(now);
                    dispatch.error_code = Some(IsolatedErrorCode::UncertainOutcome);
                    lease.state = ComputerSurfaceLeaseState::Uncertain;
                    lease.revision = lease.revision.saturating_add(1);
                    lease.updated_at = now;
                    lease.disposition =
                        Some("lease expired after injection; outcome uncertain".into());
                }
                Some(dispatch) => {
                    dispatch.state = ComputerDispatchState::KnownNotInjected;
                    dispatch.completed_at = Some(now);
                    dispatch.error_code = Some(IsolatedErrorCode::Interrupted);
                    lease.transition(
                        ComputerSurfaceLeaseState::Revoked,
                        now,
                        Some("lease expired before injection"),
                    )?;
                }
                None => {
                    lease.transition(
                        ComputerSurfaceLeaseState::Revoked,
                        now,
                        Some("lease expired"),
                    )?;
                }
            }
            self.save_lease(&lease)?;
            reaped.push(lease.lease_id.clone());
        }
        Ok(reaped)
    }

    fn guest_path(&self, guest_id: &str) -> IsolatedResult<PathBuf> {
        Ok(self
            .inner
            .root
            .join("guests")
            .join(format!("{}.json", safe_file_id(guest_id)?)))
    }

    fn lease_path(&self, lease_id: &str) -> IsolatedResult<PathBuf> {
        Ok(self
            .inner
            .root
            .join("leases")
            .join(format!("{}.json", safe_file_id(lease_id)?)))
    }

    fn recover(&self, now: DateTime<Utc>) -> IsolatedResult<RecoveryReport> {
        let mut report = RecoveryReport {
            quarantined: self.quarantine_unusable("guests")?,
            ..Default::default()
        };
        report
            .quarantined
            .extend(self.quarantine_unusable("leases")?);

        for mut guest in self.list_guests()? {
            if guest.is_live() {
                guest
                    .terminate(
                        IsolatedGuestTerminal::Interrupted,
                        now,
                        "process restart; old incarnation is not resumable",
                    )
                    .ok();
                guest.surface = guest.surface.next_incarnation();
                guest.updated_at = now;
                self.save_guest(&guest)?;
            }
        }
        for mut lease in self.list_leases()? {
            if lease.state.is_terminal() {
                continue;
            }
            match lease.dispatch.as_mut() {
                Some(dispatch) if dispatch.state == ComputerDispatchState::Injected => {
                    // Injected means the input may already have reached the
                    // guest. It becomes Uncertain and is never replayed, no
                    // matter how many restarts follow.
                    dispatch.state = ComputerDispatchState::Uncertain;
                    dispatch.completed_at = Some(now);
                    dispatch.error_code = Some(IsolatedErrorCode::UncertainOutcome);
                    lease.state = ComputerSurfaceLeaseState::Uncertain;
                    lease.revision = lease.revision.saturating_add(1);
                    lease.updated_at = now;
                    lease.disposition = Some("restart after injection; no automatic replay".into());
                    report.uncertain_after_restart.push(lease.lease_id.clone());
                }
                Some(dispatch) if dispatch.state == ComputerDispatchState::Prepared => {
                    dispatch.state = ComputerDispatchState::KnownNotInjected;
                    dispatch.completed_at = Some(now);
                    dispatch.error_code = Some(IsolatedErrorCode::Interrupted);
                    lease
                        .transition(
                            ComputerSurfaceLeaseState::Revoked,
                            now,
                            Some("restart before injection"),
                        )
                        .ok();
                }
                _ => {
                    lease
                        .transition(
                            ComputerSurfaceLeaseState::Revoked,
                            now,
                            Some("restart revoked live lease"),
                        )
                        .ok();
                }
            }
            self.save_lease(&lease)?;
        }
        report.expired_reaped = self.reap_expired(now)?;
        Ok(report)
    }

    /// Move aside anything that is not a usable record: unreadable, oversized,
    /// undeserializable, *or* deserializable but failing `validate()`.
    fn quarantine_unusable(&self, dir_name: &str) -> IsolatedResult<Vec<String>> {
        let mut moved = Vec::new();
        let dir = self.inner.root.join(dir_name);
        for path in json_paths(&dir)? {
            let usable = match fs::read(&path) {
                Ok(bytes) if bytes.len() as u64 <= MAX_RECORD_BYTES => {
                    if dir_name == "guests" {
                        serde_json::from_slice::<IsolatedGuestRecord>(&bytes)
                            .ok()
                            .is_some_and(|record| record.validate().is_ok())
                    } else {
                        serde_json::from_slice::<ComputerSurfaceLease>(&bytes)
                            .ok()
                            .is_some_and(|record| record.validate().is_ok())
                    }
                }
                _ => false,
            };
            if !usable {
                moved.push(self.quarantine_file(&path)?);
            }
        }
        Ok(moved)
    }

    fn quarantine_file(&self, path: &Path) -> IsolatedResult<String> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown.json")
            .to_string();
        let dest = self.inner.root.join("quarantine").join(&name);
        // Quarantine is itself durable: a torn quarantine that loses the file
        // would erase the evidence that something was wrong.
        fs::rename(path, &dest).map_err(io_err)?;
        sync_parent(&dest)?;
        sync_parent(path)?;
        Ok(name)
    }
}

fn json_paths(dir: &Path) -> IsolatedResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !dir.exists() {
        return Ok(paths);
    }
    for entry in fs::read_dir(dir).map_err(io_err)? {
        let path = entry.map_err(io_err)?.path();
        if path.extension().and_then(|value| value.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> IsolatedResult<Option<T>> {
    if !path.is_file() {
        return Ok(None);
    }
    Ok(Some(read_json(path)?))
}

fn read_all<T: serde::de::DeserializeOwned>(dir: &Path) -> IsolatedResult<Vec<T>> {
    let mut items = Vec::new();
    for path in json_paths(dir)? {
        items.push(read_json(&path)?);
    }
    Ok(items)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> IsolatedResult<T> {
    let metadata = fs::symlink_metadata(path).map_err(io_err)?;
    if metadata.file_type().is_symlink() {
        return Err(IsolatedError::unauthorized(
            "isolated visual record must not be a symlink",
        ));
    }
    let bytes = fs::read(path).map_err(io_err)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(IsolatedError::internal(
            "isolated visual record exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| IsolatedError::internal("isolated visual record is invalid"))
}

/// Write-temp, fsync, rename, fsync-parent. Any failure is returned; callers
/// must not proceed as though the record is durable.
fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> IsolatedResult<()> {
    let tmp = path.with_extension("json.tmp");
    let encoded = serde_json::to_vec_pretty(value)
        .map_err(|error| IsolatedError::internal(error.to_string()))?;
    {
        let mut file = fs::File::create(&tmp).map_err(io_err)?;
        file.write_all(&encoded).map_err(io_err)?;
        file.sync_all().map_err(io_err)?;
    }
    fs::rename(&tmp, path).map_err(io_err)?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> IsolatedResult<()> {
    #[cfg(unix)]
    {
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        fs::File::open(parent)
            .and_then(|file| file.sync_all())
            .map_err(io_err)?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn io_err(error: std::io::Error) -> IsolatedError {
    IsolatedError::internal(error.to_string())
}
