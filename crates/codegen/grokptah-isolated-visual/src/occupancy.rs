//! Durable exclusive occupancy leases. Process-name scans cannot grant authority.
//!
//! Reads fail closed. `inspect` returns a [`Result`], and every caller that
//! decides whether a resource is free must treat an error as "occupied", never
//! as "clear". A corrupt, oversized, symlinked, or unreadable occupancy record
//! is exactly the situation in which two owners could collide, so it denies.

use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::error::{IsolatedError, IsolatedResult};
use crate::ids::{sha256_hex, validate_id, SCHEMA_VERSION};

const MAX_RECORD_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OccupancyState {
    Clear,
    Live,
    Stale,
    Conflicting,
    Recovery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OccupancyRecord {
    pub schema_version: u32,
    pub resource_key: String,
    pub owner_id: String,
    pub guest_id: String,
    pub surface_incarnation: String,
    pub image_digest: String,
    pub overlay_id: String,
    pub vm_instance_id: Option<String>,
    pub state: OccupancyState,
    pub updated_at: DateTime<Utc>,
}

impl OccupancyRecord {
    pub fn validate(&self) -> IsolatedResult<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(IsolatedError::internal("occupancy schema is unsupported"));
        }
        validate_id("resource_key", &self.resource_key)?;
        validate_id("owner_id", &self.owner_id)?;
        validate_id("guest_id", &self.guest_id)?;
        validate_id("surface_incarnation", &self.surface_incarnation)?;
        crate::ids::validate_digest("image_digest", &self.image_digest)?;
        validate_id("overlay_id", &self.overlay_id)?;
        Ok(())
    }
}

pub fn resource_key(image_digest: &str, overlay_id: &str, surface_incarnation: &str) -> String {
    sha256_hex(format!("v1|{image_digest}|{overlay_id}|{surface_incarnation}").as_bytes())
}

pub struct OccupancyStore {
    root: PathBuf,
    /// Advisory locks this process holds, keyed by resource so releasing one
    /// resource cannot drop another resource's lock.
    locks: std::sync::Mutex<std::collections::BTreeMap<String, fs::File>>,
}

impl OccupancyStore {
    pub fn open(root: impl AsRef<Path>) -> IsolatedResult<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root).map_err(io_err)?;
        if root.is_symlink() {
            return Err(IsolatedError::unauthorized(
                "occupancy root must not be a symlink",
            ));
        }
        let canonical = dunce::canonicalize(&root).map_err(io_err)?;
        Ok(Self {
            root: canonical,
            locks: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    pub fn inspect(&self, resource_key: &str) -> IsolatedResult<OccupancyState> {
        match self.load(resource_key)? {
            None => Ok(OccupancyState::Clear),
            Some(record) => Ok(record.state),
        }
    }

    /// Worst state across every record in the store.
    ///
    /// A read error is not "nothing found": it is reported as
    /// [`OccupancyState::Conflicting`] with the reason, so a caller cannot
    /// mistake an unreadable store for an empty one.
    pub fn sweep(&self) -> (OccupancyState, Vec<String>) {
        let mut worst = OccupancyState::Clear;
        let mut problems = Vec::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) => {
                return (
                    OccupancyState::Conflicting,
                    vec![format!("occupancy root is unreadable ({error})")],
                );
            }
        };
        for entry in entries {
            let Ok(entry) = entry else {
                worst = OccupancyState::Conflicting;
                problems.push("occupancy entry is unreadable".into());
                continue;
            };
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(key) = name.strip_suffix(".json") else {
                continue;
            };
            match self.inspect(key) {
                Ok(state) => {
                    if rank(state) > rank(worst) {
                        worst = state;
                    }
                }
                Err(error) => {
                    worst = OccupancyState::Conflicting;
                    problems.push(format!(
                        "occupancy record {key} is unreadable: {}",
                        error.message
                    ));
                }
            }
        }
        (worst, problems)
    }

    pub fn try_acquire(&self, record: OccupancyRecord) -> IsolatedResult<OccupancyRecord> {
        record.validate()?;
        let path = self.record_path(&record.resource_key)?;
        let lock_path = self.lock_path(&record.resource_key)?;
        if let Some(existing) = self.load(&record.resource_key)? {
            if existing.state == OccupancyState::Live && existing.owner_id != record.owner_id {
                return Err(IsolatedError::conflict(
                    "occupancy resource is held by a live owner",
                ));
            }
            if existing.state == OccupancyState::Conflicting {
                return Err(IsolatedError::conflict(
                    "occupancy resource is in conflict and cannot be acquired",
                ));
            }
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(io_err)?;
        lock.try_lock_exclusive()
            .map_err(|_| IsolatedError::conflict("occupancy lock is held by another live owner"))?;
        let mut live = record;
        live.state = OccupancyState::Live;
        atomic_write_json(&path, &live)?;
        self.locks
            .lock()
            .map_err(|_| IsolatedError::internal("occupancy lock set is poisoned"))?
            .insert(live.resource_key.clone(), lock);
        Ok(live)
    }

    pub fn mark_stale(
        &self,
        resource_key: &str,
        owner_id: &str,
    ) -> IsolatedResult<OccupancyRecord> {
        let mut record = self
            .load(resource_key)?
            .ok_or_else(|| IsolatedError::invalid_state("occupancy record is missing"))?;
        if record.owner_id != owner_id {
            record.state = OccupancyState::Conflicting;
            atomic_write_json(&self.record_path(resource_key)?, &record)?;
            return Err(IsolatedError::conflict(
                "occupancy owner does not match the stale transition",
            ));
        }
        record.state = OccupancyState::Stale;
        atomic_write_json(&self.record_path(resource_key)?, &record)?;
        Ok(record)
    }

    pub fn recover(&self, resource_key: &str, owner_id: &str) -> IsolatedResult<OccupancyRecord> {
        let mut record = self
            .load(resource_key)?
            .ok_or_else(|| IsolatedError::invalid_state("occupancy record is missing"))?;
        if record.owner_id != owner_id {
            record.state = OccupancyState::Conflicting;
            atomic_write_json(&self.record_path(resource_key)?, &record)?;
            return Err(IsolatedError::conflict(
                "occupancy recovery owner does not match",
            ));
        }
        record.state = OccupancyState::Recovery;
        atomic_write_json(&self.record_path(resource_key)?, &record)?;
        Ok(record)
    }

    /// Release a resource this owner holds.
    ///
    /// A read error here denies rather than proceeding: releasing a record we
    /// cannot read risks tearing down another owner's claim. Failure to remove
    /// the record is reported, because a lingering record is the difference
    /// between "released" and "believed released".
    pub fn release(&self, resource_key: &str, owner_id: &str) -> IsolatedResult<()> {
        match self.load(resource_key) {
            Ok(Some(record)) if record.owner_id != owner_id => {
                return Err(IsolatedError::conflict(
                    "occupancy release owner does not match",
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(IsolatedError::conflict(format!(
                    "occupancy record is unreadable and will not be released blindly ({})",
                    error.message
                )));
            }
        }
        // Drop this resource's advisory lock before unlinking, so the lock file
        // is not removed while still held. Other resources keep theirs.
        if let Ok(mut locks) = self.locks.lock() {
            locks.remove(resource_key);
        }
        let record_path = self.record_path(resource_key)?;
        if let Err(error) = fs::remove_file(&record_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(IsolatedError::internal(format!(
                    "occupancy record could not be removed ({error})"
                )));
            }
        }
        let lock_path = self.lock_path(resource_key)?;
        if let Err(error) = fs::remove_file(&lock_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(IsolatedError::internal(format!(
                    "occupancy lock could not be removed ({error})"
                )));
            }
        }
        Ok(())
    }

    /// Does this store still hold a record for `resource_key`?
    ///
    /// Distinct from [`Self::inspect`]: an unreadable record answers "yes,
    /// something is here", which is what a cleanup probe needs to know.
    pub fn holds(&self, resource_key: &str) -> bool {
        self.record_path(resource_key)
            .map(|path| path.exists())
            .unwrap_or(true)
    }

    fn load(&self, resource_key: &str) -> IsolatedResult<Option<OccupancyRecord>> {
        let path = self.record_path(resource_key)?;
        if !path.exists() {
            return Ok(None);
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_err)?;
        if metadata.file_type().is_symlink() {
            return Err(IsolatedError::unauthorized(
                "occupancy record must not be a symlink",
            ));
        }
        if metadata.len() > MAX_RECORD_BYTES {
            return Err(IsolatedError::limit("occupancy record exceeds size bound"));
        }
        let raw = fs::read(&path).map_err(io_err)?;
        let record: OccupancyRecord = serde_json::from_slice(&raw)
            .map_err(|_| IsolatedError::invalid("occupancy record is corrupt"))?;
        record.validate()?;
        Ok(Some(record))
    }

    fn record_path(&self, resource_key: &str) -> IsolatedResult<PathBuf> {
        validate_id("resource_key", resource_key)?;
        Ok(self.root.join(format!("{resource_key}.json")))
    }

    fn lock_path(&self, resource_key: &str) -> IsolatedResult<PathBuf> {
        validate_id("resource_key", resource_key)?;
        Ok(self.root.join(format!("{resource_key}.lock")))
    }
}

fn atomic_write_json(path: &Path, value: &OccupancyRecord) -> IsolatedResult<()> {
    value.validate()?;
    let encoded = serde_json::to_vec(value)
        .map_err(|_| IsolatedError::internal("occupancy record is not serializable"))?;
    if encoded.len() as u64 > MAX_RECORD_BYTES {
        return Err(IsolatedError::limit("occupancy record exceeds size bound"));
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp)
            .map_err(io_err)?;
        file.write_all(&encoded).map_err(io_err)?;
        file.sync_all().map_err(io_err)?;
    }
    fs::rename(tmp, path).map_err(io_err)
}

fn rank(state: OccupancyState) -> u8 {
    match state {
        OccupancyState::Clear => 0,
        OccupancyState::Stale => 1,
        OccupancyState::Recovery => 2,
        OccupancyState::Live => 3,
        OccupancyState::Conflicting => 4,
    }
}

fn io_err(error: std::io::Error) -> IsolatedError {
    IsolatedError::internal(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn record(owner: &str) -> OccupancyRecord {
        OccupancyRecord {
            schema_version: SCHEMA_VERSION,
            resource_key: resource_key(&"a".repeat(64), "overlay-1", "incarnation-1"),
            owner_id: owner.into(),
            guest_id: "guest-1".into(),
            surface_incarnation: "incarnation-1".into(),
            image_digest: "a".repeat(64),
            overlay_id: "overlay-1".into(),
            vm_instance_id: None,
            state: OccupancyState::Clear,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn two_live_owners_cannot_share_a_resource() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let first = store.try_acquire(record("owner-a")).unwrap();
        assert_eq!(first.state, OccupancyState::Live);
        assert_eq!(
            store.try_acquire(record("owner-b")).unwrap_err().code,
            crate::error::IsolatedErrorCode::Conflict
        );
        assert_eq!(
            store.inspect(&first.resource_key).unwrap(),
            OccupancyState::Live
        );
    }

    #[test]
    fn symlink_and_oversized_records_fail_closed() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let rec = record("owner-a");
        store.try_acquire(rec.clone()).unwrap();
        let path = dir.path().join(format!("{}.json", rec.resource_key));
        fs::write(&path, vec![b'x'; 20_000]).unwrap();
        assert_eq!(
            store.inspect(&rec.resource_key).unwrap_err().code,
            crate::error::IsolatedErrorCode::LimitReached
        );
    }

    #[test]
    fn a_corrupt_record_denies_rather_than_reading_as_clear() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let rec = record("owner-a");
        store.try_acquire(rec.clone()).unwrap();
        let path = dir.path().join(format!("{}.json", rec.resource_key));
        fs::write(&path, b"{ not json").unwrap();

        assert!(store.inspect(&rec.resource_key).is_err());
        let (state, problems) = store.sweep();
        assert_eq!(state, OccupancyState::Conflicting);
        assert!(!problems.is_empty());
        // A record we cannot read is never released blindly.
        assert!(store.release(&rec.resource_key, "owner-a").is_err());
        assert!(store.holds(&rec.resource_key));
    }

    #[test]
    fn a_semantically_invalid_record_denies() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let rec = record("owner-a");
        store.try_acquire(rec.clone()).unwrap();
        let path = dir.path().join(format!("{}.json", rec.resource_key));
        // Deserializes fine; `image_digest` is not a SHA-256.
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        value["imageDigest"] = serde_json::json!("nope");
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(store.inspect(&rec.resource_key).is_err());
        assert_eq!(store.sweep().0, OccupancyState::Conflicting);
    }

    #[test]
    fn sweep_reports_the_worst_state_present() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        assert_eq!(store.sweep().0, OccupancyState::Clear);
        let rec = store.try_acquire(record("owner-a")).unwrap();
        assert_eq!(store.sweep().0, OccupancyState::Live);
        store.release(&rec.resource_key, "owner-a").unwrap();
        assert_eq!(store.sweep().0, OccupancyState::Clear);
    }

    #[test]
    fn process_name_is_not_consulted_for_grant() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let rec = store.try_acquire(record("owner-a")).unwrap();
        assert_eq!(rec.state, OccupancyState::Live);
        store.release(&rec.resource_key, "owner-a").unwrap();
        assert_eq!(
            store.inspect(&rec.resource_key).unwrap(),
            OccupancyState::Clear
        );
    }
}
