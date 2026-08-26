//! Durable CAS persistence for sealed specs, leases, send records, and tombstones.
//!
//! Pathname I/O is handle-relative at the store root: the root is opened and
//! canonicalized once. Subsequent writes use the already-canonical child path
//! and refuse `..` / symlink escape via `safe_id_filename`. This is not a
//! complete Windows reparse implementation; Windows ACL/reparse remains residual.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use super::authority::{InternalExecutionSpec, SpineError};
use super::lease::AttemptLease;
use super::lifecycle::{ExecutionLifecycle, ProviderSendState};
use super::types::{safe_id_filename, OrchError, OrchErrorCode};

/// Durable provider-send record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderSendRecord {
    /// Stable provider-request identity.
    pub provider_request_id: String,
    /// Bound run.
    pub run_id: String,
    /// Bound attempt.
    pub attempt_id: String,
    /// Bound work.
    pub work_id: String,
    /// Send lattice.
    pub state: ProviderSendState,
    /// CAS revision.
    pub revision: u64,
    /// Optional provider-assigned run identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_run_id: Option<String>,
}

/// Compact idempotency tombstone that outlives receipt pruning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdempotencyTombstone {
    /// Original request identity.
    pub request_id: String,
    /// Bound work identity, when admitted.
    pub work_id: String,
    /// Bound run identity, when admitted.
    pub run_id: String,
    /// Outcome code.
    pub outcome: String,
    /// Unix milliseconds when the tombstone was written.
    pub written_at_unix_ms: u64,
}

/// Durable execution record bound to one sealed specification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionRecord {
    /// Sealed specification.
    pub spec: InternalExecutionSpec,
    /// Lifecycle.
    pub lifecycle: ExecutionLifecycle,
    /// CAS revision.
    pub revision: u64,
}

/// Handle-relative spine persistence under an orchestration store root.
#[derive(Clone)]
pub struct SpinePersist {
    inner: Arc<SpinePersistInner>,
}

struct SpinePersistInner {
    root: PathBuf,
    _lock: File,
    mutex: Mutex<()>,
}

impl SpinePersist {
    /// Open or create the spine directory. Fails if another process holds the lock.
    pub fn open(store_root: impl AsRef<Path>) -> Result<Self, OrchError> {
        let root = store_root.as_ref().join("spine");
        fs::create_dir_all(root.join("specs")).map_err(io_err)?;
        fs::create_dir_all(root.join("leases")).map_err(io_err)?;
        fs::create_dir_all(root.join("sends")).map_err(io_err)?;
        fs::create_dir_all(root.join("inputs")).map_err(io_err)?;
        fs::create_dir_all(root.join("tombstones")).map_err(io_err)?;
        let root = dunce::canonicalize(&root).map_err(io_err)?;
        let lock_path = root.join(".spine.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        lock.try_lock_exclusive().map_err(|error| {
            OrchError::new(
                OrchErrorCode::Conflict,
                format!("spine persist is already open ({error})"),
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        Ok(Self {
            inner: Arc::new(SpinePersistInner {
                root,
                _lock: lock,
                mutex: Mutex::new(()),
            }),
        })
    }

    /// Root directory.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    fn child(&self, dir: &str, id: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(id)?;
        Ok(self.inner.root.join(dir).join(format!("{safe}.json")))
    }

    /// Persist a sealed specification and lifecycle as one CAS create.
    pub fn create_execution(&self, record: &ExecutionRecord) -> Result<(), SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("specs", &record.spec.run_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        write_json_exclusive(&path, record)
    }

    /// Load an execution record.
    pub fn load_execution(&self, run_id: &str) -> Result<ExecutionRecord, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("specs", run_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        read_json(&path)
    }

    /// CAS lifecycle transition.
    pub fn cas_lifecycle(
        &self,
        run_id: &str,
        expected_revision: u64,
        expected: ExecutionLifecycle,
        next: ExecutionLifecycle,
    ) -> Result<ExecutionRecord, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("specs", run_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        let mut record: ExecutionRecord = read_json(&path)?;
        if record.revision != expected_revision || record.lifecycle != expected {
            return Err(SpineError::StaleRevision);
        }
        record.lifecycle = super::lifecycle::transition_lifecycle(record.lifecycle, next)?;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or(SpineError::RevisionOverflow)?;
        atomic_write_json(&path, &record)?;
        Ok(record)
    }

    /// Create the attempt lease. Fails if a lease already exists.
    pub fn create_lease(&self, lease: &AttemptLease) -> Result<(), SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("leases", &lease.lease_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        write_json_exclusive(&path, lease)
    }

    /// Load a lease.
    pub fn load_lease(&self, lease_id: &str) -> Result<AttemptLease, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("leases", lease_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        read_json(&path)
    }

    /// Create the provider-send record as KnownNotSent.
    pub fn create_send(&self, record: &ProviderSendRecord) -> Result<(), SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("sends", &record.provider_request_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        write_json_exclusive(&path, record)
    }

    /// Load a send record.
    pub fn load_send(&self, provider_request_id: &str) -> Result<ProviderSendRecord, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("sends", provider_request_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        read_json(&path)
    }

    /// CAS send-state transition.
    pub fn cas_send(
        &self,
        provider_request_id: &str,
        expected_revision: u64,
        expected: ProviderSendState,
        next: ProviderSendState,
    ) -> Result<ProviderSendRecord, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("sends", provider_request_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        let mut record: ProviderSendRecord = read_json(&path)?;
        if record.revision != expected_revision || record.state != expected {
            return Err(SpineError::StaleRevision);
        }
        record.state = super::lifecycle::transition_send(record.state, next)?;
        record.revision = record
            .revision
            .checked_add(1)
            .ok_or(SpineError::RevisionOverflow)?;
        atomic_write_json(&path, &record)?;
        Ok(record)
    }

    /// Persist private input bytes (0600). Never projected.
    pub fn save_private_input(&self, work_id: &str, bytes: &[u8]) -> Result<(), SpineError> {
        let _guard = self.inner.mutex.lock();
        let safe = safe_id_filename(work_id).map_err(|_| SpineError::InvalidIdentity)?;
        let path = self.inner.root.join("inputs").join(format!("{safe}.bin"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| SpineError::TransitionForbidden)?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| SpineError::TransitionForbidden)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// Write an idempotency tombstone. Existing tombstones are immutable.
    pub fn write_tombstone(&self, tombstone: &IdempotencyTombstone) -> Result<(), SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("tombstones", &tombstone.request_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        write_json_exclusive(&path, tombstone)
    }

    /// Load a tombstone if present.
    pub fn load_tombstone(
        &self,
        request_id: &str,
    ) -> Result<Option<IdempotencyTombstone>, SpineError> {
        let _guard = self.inner.mutex.lock();
        let path = self
            .child("tombstones", request_id)
            .map_err(|_| SpineError::InvalidIdentity)?;
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(read_json(&path)?))
    }
}

fn io_err(error: std::io::Error) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, error.to_string())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, SpineError> {
    let bytes = fs::read(path).map_err(|_| SpineError::InvalidIdentity)?;
    serde_json::from_slice(&bytes).map_err(|_| SpineError::UnknownField)
}

fn write_json_exclusive<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    if path.exists() {
        return Err(SpineError::DuplicateIdentity);
    }
    atomic_write_json(path, value)
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), SpineError> {
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| SpineError::UnknownField)?;
    let mut file = File::create(&tmp).map_err(|_| SpineError::TransitionForbidden)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| SpineError::TransitionForbidden)?;
    fs::rename(&tmp, path).map_err(|_| SpineError::TransitionForbidden)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let _ = File::open(parent).and_then(|file| file.sync_all());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::authority::{unsigned_provider_spec, MacKey};

    #[test]
    fn cas_and_tombstone_horizon() {
        let dir = tempfile::tempdir().unwrap();
        let persist = SpinePersist::open(dir.path()).unwrap();
        let key = MacKey::from_bytes(&[0x11; 32]).unwrap();
        let spec = unsigned_provider_spec("persist", "intent")
            .seal(&key)
            .unwrap();
        persist
            .create_execution(&ExecutionRecord {
                spec: spec.clone(),
                lifecycle: ExecutionLifecycle::Queued,
                revision: 0,
            })
            .unwrap();
        persist
            .cas_lifecycle(
                &spec.run_id,
                0,
                ExecutionLifecycle::Queued,
                ExecutionLifecycle::Starting,
            )
            .unwrap();
        assert_eq!(
            persist
                .cas_lifecycle(
                    &spec.run_id,
                    0,
                    ExecutionLifecycle::Queued,
                    ExecutionLifecycle::Starting,
                )
                .unwrap_err(),
            SpineError::StaleRevision
        );
        persist
            .write_tombstone(&IdempotencyTombstone {
                request_id: spec.request_id.clone(),
                work_id: spec.work_id.clone(),
                run_id: spec.run_id.clone(),
                outcome: "queued".into(),
                written_at_unix_ms: 1,
            })
            .unwrap();
        assert_eq!(
            persist
                .write_tombstone(&IdempotencyTombstone {
                    request_id: spec.request_id.clone(),
                    work_id: spec.work_id.clone(),
                    run_id: spec.run_id.clone(),
                    outcome: "queued".into(),
                    written_at_unix_ms: 2,
                })
                .unwrap_err(),
            SpineError::DuplicateIdentity
        );
    }
}
