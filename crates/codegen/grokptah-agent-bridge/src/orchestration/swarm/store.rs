//! Durable persistence for work graphs, layered on the existing orchestration
//! ledger.
//!
//! This is not a second store. Graph state lives under the same `OrchStore`
//! root, behind the same exclusive store lock, and uses the same atomic-write
//! and exclusive-create primitives as runs and idempotency receipts. What it
//! adds is compare-and-swap on the graph revision and one-winner claims for
//! leases, authorities, and Computer Use grant consumption.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::orchestration::store::OrchStore;
use crate::orchestration::types::{safe_id_filename, OrchError, OrchErrorCode};

use super::ids::{AuthorityId, GrantId, GraphId, LeaseId};
use super::state::WorkGraphRecord;

const GRAPH_DIR: &str = "swarm/graphs";
const LEASE_CLAIM_DIR: &str = "swarm/lease-claims";
const AUTHORITY_CLAIM_DIR: &str = "swarm/authority-claims";
const GRANT_CLAIM_DIR: &str = "swarm/grant-claims";

fn internal(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, message)
}

fn conflict(message: impl Into<String>) -> OrchError {
    OrchError::new(OrchErrorCode::Conflict, message)
}

/// Durable outcome of a one-winner claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This caller won.
    Won,
    /// Someone else already holds it.
    AlreadyHeld,
}

/// One durable claim record. Deliberately tiny and secret-free.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaimRecord {
    pub key: String,
    pub holder: String,
    pub claimed_at: DateTime<Utc>,
}

/// Graph persistence and claim primitives over an existing `OrchStore`.
///
/// Holding the `OrchStore` rather than a path is deliberate: the process-wide
/// exclusive lock the store already took is what makes these files safe to
/// share between the run ledger and the graph ledger.
#[derive(Clone)]
pub struct SwarmStore {
    store: OrchStore,
}

impl SwarmStore {
    pub fn new(store: OrchStore) -> Self {
        Self { store }
    }

    pub fn orch_store(&self) -> &OrchStore {
        &self.store
    }

    fn root(&self) -> &Path {
        self.store.root()
    }

    fn path_in(&self, dir: &str, id: &str, extension: &str) -> Result<PathBuf, OrchError> {
        let safe = safe_id_filename(id)?;
        Ok(self.root().join(dir).join(format!("{safe}.{extension}")))
    }

    fn graph_path(&self, graph_id: &GraphId) -> Result<PathBuf, OrchError> {
        self.path_in(GRAPH_DIR, graph_id.as_str(), "json")
    }

    /// Persist a brand-new graph. Fails if one already exists for this id.
    pub fn create_graph(&self, record: &WorkGraphRecord) -> Result<(), OrchError> {
        record.validate()?;
        let path = self.graph_path(&record.graph_id)?;
        match write_json_exclusive(&path, record) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(conflict("a work graph already exists for this id"))
            }
            Err(error) => Err(internal(error.to_string())),
        }
    }

    /// Load and validate a graph. A malformed durable record fails closed.
    pub fn load_graph(&self, graph_id: &GraphId) -> Result<Option<WorkGraphRecord>, OrchError> {
        let path = self.graph_path(graph_id)?;
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path).map_err(|error| internal(error.to_string()))?;
        let record: WorkGraphRecord = serde_json::from_str(&text).map_err(|error| {
            OrchError::new(
                OrchErrorCode::Internal,
                format!("work graph record is malformed: {error}"),
            )
        })?;
        record.validate()?;
        if &record.graph_id != graph_id {
            return Err(OrchError::new(
                OrchErrorCode::Internal,
                "work graph record does not match the requested id",
            ));
        }
        Ok(Some(record))
    }

    /// Compare-and-swap the graph on its revision.
    ///
    /// The caller passes the revision it read. A concurrent writer that already
    /// advanced the record makes this a stale-version refusal rather than a
    /// silent overwrite.
    pub fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: &WorkGraphRecord,
        now: DateTime<Utc>,
    ) -> Result<WorkGraphRecord, OrchError> {
        let current = self.load_graph(&next.graph_id)?.ok_or_else(|| {
            OrchError::new(OrchErrorCode::InvalidRequest, "work graph does not exist")
        })?;
        if current.revision != expected_revision {
            return Err(OrchError::new(
                OrchErrorCode::StaleVersion,
                format!(
                    "work graph revision moved from {expected_revision} to {}",
                    current.revision
                ),
            ));
        }
        if current.session_id != next.session_id || current.workspace != next.workspace {
            return Err(OrchError::new(
                OrchErrorCode::WorkspaceMismatch,
                "work graph identity cannot be rebound by an update",
            ));
        }
        let mut committed = next.clone();
        committed.revision = expected_revision.checked_add(1).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::CapacityExhausted,
                "work graph revision space is exhausted",
            )
        })?;
        committed.updated_at = committed.updated_at.max(now);
        committed.validate()?;
        let path = self.graph_path(&committed.graph_id)?;
        atomic_write_json(&path, &committed).map_err(|error| internal(error.to_string()))?;
        Ok(committed)
    }

    /// Read every graph in the ledger, skipping unreadable records.
    ///
    /// A malformed record is reported by count rather than being silently
    /// dropped, so a broken ledger cannot look healthy.
    pub fn list_graphs(&self) -> Result<(Vec<WorkGraphRecord>, usize), OrchError> {
        let dir = self.root().join(GRAPH_DIR);
        if !dir.is_dir() {
            return Ok((Vec::new(), 0));
        }
        let mut records = Vec::new();
        let mut skipped = 0usize;
        let entries = fs::read_dir(&dir).map_err(|error| internal(error.to_string()))?;
        for entry in entries {
            let Ok(entry) = entry else {
                skipped += 1;
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            match fs::read_to_string(&path)
                .ok()
                .and_then(|text| serde_json::from_str::<WorkGraphRecord>(&text).ok())
            {
                // The filename is derived from the graph id, so a record whose
                // id no longer matches its own file has been moved or rewritten.
                // Counting it as unreadable is what keeps a tampered ledger from
                // looking healthy.
                Some(record)
                    if record.validate().is_ok()
                        && safe_id_filename(record.graph_id.as_str())
                            .is_ok_and(|expected| expected == stem) =>
                {
                    records.push(record)
                }
                _ => skipped += 1,
            }
        }
        records.sort_by(|left, right| left.graph_id.as_str().cmp(right.graph_id.as_str()));
        Ok((records, skipped))
    }

    fn claim(&self, dir: &str, key: &str, holder: &str) -> Result<ClaimOutcome, OrchError> {
        let path = self.path_in(dir, key, "claim")?;
        let record = ClaimRecord {
            key: key.to_string(),
            holder: holder.to_string(),
            claimed_at: Utc::now(),
        };
        match write_json_exclusive(&path, &record) {
            Ok(()) => Ok(ClaimOutcome::Won),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(ClaimOutcome::AlreadyHeld)
            }
            Err(error) => Err(internal(error.to_string())),
        }
    }

    fn claim_holder(&self, dir: &str, key: &str) -> Result<Option<String>, OrchError> {
        let path = self.path_in(dir, key, "claim")?;
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(&path).map_err(|error| internal(error.to_string()))?;
        let record: ClaimRecord =
            serde_json::from_str(&text).map_err(|error| internal(error.to_string()))?;
        Ok(Some(record.holder))
    }

    /// Win the exclusive right to spawn one lease's child.
    ///
    /// Exactly one caller wins per lease, so a replayed dispatch request can
    /// never authorize a second child.
    pub fn claim_lease_spawn(
        &self,
        lease_id: &LeaseId,
        holder: &str,
    ) -> Result<ClaimOutcome, OrchError> {
        self.claim(LEASE_CLAIM_DIR, lease_id.as_str(), holder)
    }

    pub fn lease_spawn_holder(&self, lease_id: &LeaseId) -> Result<Option<String>, OrchError> {
        self.claim_holder(LEASE_CLAIM_DIR, lease_id.as_str())
    }

    /// Consume a single-use authority. The second consumer loses.
    pub fn consume_authority(
        &self,
        authority_id: &AuthorityId,
        holder: &str,
    ) -> Result<ClaimOutcome, OrchError> {
        self.claim(AUTHORITY_CLAIM_DIR, authority_id.as_str(), holder)
    }

    pub fn authority_holder(
        &self,
        authority_id: &AuthorityId,
    ) -> Result<Option<String>, OrchError> {
        self.claim_holder(AUTHORITY_CLAIM_DIR, authority_id.as_str())
    }

    /// Consume a Computer Use grant for one exact epoch.
    ///
    /// The key deliberately includes the control epoch: a pause, takeover,
    /// stop, or recovery bumps the epoch, so a binding captured before the
    /// takeover can never consume the grant afterwards, and the new owner's
    /// binding is a distinct key rather than a contested one.
    pub fn consume_grant(
        &self,
        grant_id: &GrantId,
        control_epoch: u64,
        holder: &str,
    ) -> Result<ClaimOutcome, OrchError> {
        let key = format!("{grant_id}:{control_epoch}");
        self.claim(GRANT_CLAIM_DIR, &key, holder)
    }

    pub fn grant_holder(
        &self,
        grant_id: &GrantId,
        control_epoch: u64,
    ) -> Result<Option<String>, OrchError> {
        let key = format!("{grant_id}:{control_epoch}");
        self.claim_holder(GRANT_CLAIM_DIR, &key)
    }
}

fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    use std::io::Write;
    let mut file = fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn write_json_exclusive<T: serde::Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?)?;
    file.sync_all()?;
    Ok(())
}
