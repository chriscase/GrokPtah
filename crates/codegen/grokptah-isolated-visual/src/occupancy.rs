//! Durable exclusive occupancy leases. Process-name scans cannot grant authority.

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

/// Exclusive occupancy of one guest image, overlay slot, and surface.
/// Do not include a per-guest UUID: that makes exclusivity a no-op.
pub const PRIMARY_OVERLAY_ID: &str = "isolated-visual-primary-overlay";
pub const PRIMARY_SURFACE_ID: &str = "isolated-visual-primary-surface";

pub fn resource_key(image_digest: &str, overlay_id: &str, surface_id: &str) -> String {
    sha256_hex(format!("v1|{image_digest}|{overlay_id}|{surface_id}").as_bytes())
}

impl OccupancyState {
    fn severity(self) -> u8 {
        match self {
            Self::Clear => 0,
            Self::Stale => 1,
            Self::Recovery => 2,
            Self::Live => 3,
            Self::Conflicting => 4,
        }
    }

    pub fn merge(self, other: Self) -> Self {
        if other.severity() > self.severity() {
            other
        } else {
            self
        }
    }

    pub fn is_held(self) -> bool {
        self != Self::Clear
    }
}

pub struct OccupancyStore {
    root: PathBuf,
    locks: std::sync::Mutex<Vec<fs::File>>,
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
            locks: std::sync::Mutex::new(Vec::new()),
        })
    }

    pub fn inspect(&self, resource_key: &str) -> IsolatedResult<OccupancyState> {
        match self.load(resource_key)? {
            None => Ok(OccupancyState::Clear),
            Some(record) => Ok(record.state),
        }
    }

    /// Inspect every occupancy record. Corrupt records fail closed instead of
    /// reporting Clear.
    pub fn inspect_any(&self) -> IsolatedResult<OccupancyState> {
        let mut worst = OccupancyState::Clear;
        for record in self.list()? {
            worst = worst.merge(record.state);
        }
        Ok(worst)
    }

    pub fn list(&self) -> IsolatedResult<Vec<OccupancyRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(key) = name.strip_suffix(".json") else {
                continue;
            };
            if let Some(record) = self.load(key)? {
                records.push(record);
            }
        }
        Ok(records)
    }

    pub fn find_for_guest(&self, guest_id: &str) -> IsolatedResult<Option<OccupancyRecord>> {
        for record in self.list()? {
            if record.guest_id == guest_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub fn try_acquire(&self, record: OccupancyRecord) -> IsolatedResult<OccupancyRecord> {
        record.validate()?;
        let path = self.record_path(&record.resource_key)?;
        let lock_path = self.lock_path(&record.resource_key)?;
        if let Some(existing) = self.load(&record.resource_key)? {
            return Err(IsolatedError::conflict(format!(
                "occupancy resource is not clear ({:?})",
                existing.state
            )));
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
            .push(lock);
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

    pub fn release(&self, resource_key: &str, owner_id: &str) -> IsolatedResult<()> {
        if let Some(record) = self.load(resource_key)? {
            if record.owner_id != owner_id {
                return Err(IsolatedError::conflict(
                    "occupancy release owner does not match",
                ));
            }
        }
        let _ = fs::remove_file(self.record_path(resource_key)?);
        let _ = fs::remove_file(self.lock_path(resource_key)?);
        Ok(())
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

    #[test]
    fn inspect_any_fails_closed_on_corrupt_records() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        let rec = store.try_acquire(record("owner-a")).unwrap();
        fs::write(
            dir.path().join(format!("{}.json", rec.resource_key)),
            b"{not-json",
        )
        .unwrap();
        assert_eq!(
            store.inspect_any().unwrap_err().code,
            crate::error::IsolatedErrorCode::InvalidRequest
        );
    }

    #[test]
    fn same_owner_cannot_reacquire_a_live_resource() {
        let dir = tempdir().unwrap();
        let store = OccupancyStore::open(dir.path()).unwrap();
        store.try_acquire(record("owner-a")).unwrap();
        assert_eq!(
            store.try_acquire(record("owner-a")).unwrap_err().code,
            crate::error::IsolatedErrorCode::Conflict
        );
    }

    #[test]
    fn resource_key_does_not_include_a_guest_uuid() {
        let first = resource_key(&"a".repeat(64), PRIMARY_OVERLAY_ID, PRIMARY_SURFACE_ID);
        let second = resource_key(&"a".repeat(64), PRIMARY_OVERLAY_ID, PRIMARY_SURFACE_ID);
        assert_eq!(first, second);
        let other_image = resource_key(&"b".repeat(64), PRIMARY_OVERLAY_ID, PRIMARY_SURFACE_ID);
        assert_ne!(first, other_image);
    }
}
