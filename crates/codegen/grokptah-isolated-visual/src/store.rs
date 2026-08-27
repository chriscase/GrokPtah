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

pub struct IsolatedVisualStore {
    inner: Arc<IsolatedVisualStoreInner>,
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
        let store = Self {
            inner: Arc::new(IsolatedVisualStoreInner {
                root,
                _store_lock: store_lock,
            }),
        };
        store.recover(now)?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn save_guest(&self, guest: &IsolatedGuestRecord) -> IsolatedResult<()> {
        guest.validate()?;
        atomic_write_json(&self.guest_path(&guest.guest_id)?, guest)
    }

    pub fn load_guest(&self, guest_id: &str) -> IsolatedResult<Option<IsolatedGuestRecord>> {
        read_optional(&self.guest_path(guest_id)?)
    }

    pub fn list_guests(&self) -> IsolatedResult<Vec<IsolatedGuestRecord>> {
        read_all(&self.inner.root.join("guests"))
    }

    pub fn save_lease(&self, lease: &ComputerSurfaceLease) -> IsolatedResult<()> {
        lease.validate()?;
        atomic_write_json(&self.lease_path(&lease.lease_id)?, lease)
    }

    pub fn load_lease(&self, lease_id: &str) -> IsolatedResult<Option<ComputerSurfaceLease>> {
        read_optional(&self.lease_path(lease_id)?)
    }

    pub fn list_leases(&self) -> IsolatedResult<Vec<ComputerSurfaceLease>> {
        read_all(&self.inner.root.join("leases"))
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

    fn recover(&self, now: DateTime<Utc>) -> IsolatedResult<()> {
        self.quarantine_unreadable("guests")?;
        self.quarantine_unreadable("leases")?;
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
                    dispatch.state = ComputerDispatchState::Uncertain;
                    dispatch.completed_at = Some(now);
                    dispatch.error_code = Some(IsolatedErrorCode::UncertainOutcome);
                    lease.state = ComputerSurfaceLeaseState::Uncertain;
                    lease.revision = lease.revision.saturating_add(1);
                    lease.updated_at = now;
                    lease.disposition = Some("restart after injection; no automatic replay".into());
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
        Ok(())
    }

    fn quarantine_unreadable(&self, dir_name: &str) -> IsolatedResult<()> {
        let dir = self.inner.root.join(dir_name);
        for path in json_paths(&dir)? {
            match fs::read(&path) {
                Ok(bytes) if bytes.len() as u64 <= MAX_RECORD_BYTES => {
                    let typed_ok = if dir_name == "guests" {
                        serde_json::from_slice::<IsolatedGuestRecord>(&bytes).is_ok()
                    } else {
                        serde_json::from_slice::<ComputerSurfaceLease>(&bytes).is_ok()
                    };
                    if !typed_ok {
                        self.quarantine_file(&path)?;
                    }
                }
                _ => self.quarantine_file(&path)?,
            }
        }
        Ok(())
    }

    fn quarantine_file(&self, path: &Path) -> IsolatedResult<()> {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown.json");
        let dest = self.inner.root.join("quarantine").join(name);
        fs::rename(path, dest).map_err(io_err)
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
    let bytes = fs::read(path).map_err(io_err)?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(IsolatedError::internal(
            "isolated visual record exceeds size limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| IsolatedError::internal("isolated visual record is invalid"))
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> IsolatedResult<()> {
    let tmp = path.with_extension("json.tmp");
    let mut file = fs::File::create(&tmp).map_err(io_err)?;
    file.write_all(
        &serde_json::to_vec_pretty(value)
            .map_err(|error| IsolatedError::internal(error.to_string()))?,
    )
    .map_err(io_err)?;
    file.sync_all().map_err(io_err)?;
    fs::rename(&tmp, path).map_err(io_err)?;
    #[cfg(unix)]
    fs::File::open(path.parent().expect("record path has parent"))
        .and_then(|file| file.sync_all())
        .map_err(io_err)?;
    Ok(())
}

fn io_err(error: std::io::Error) -> IsolatedError {
    IsolatedError::internal(error.to_string())
}
