//! Durable snapshot for restart recovery tests.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::channels::ChannelRegistry;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::GuestLifecycle;
use crate::sentinel::HostSentinelSnapshot;

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const SNAPSHOT_FILE: &str = "isolated_surface_snapshot.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSnapshot {
    pub schema_version: u32,
    pub lifecycle: GuestLifecycle,
    pub host_baseline: HostSentinelSnapshot,
    pub channels: ChannelRegistry,
    pub auto_retry_attempts: u32,
    pub saved_at: DateTime<Utc>,
}

impl HarnessSnapshot {
    pub fn new(
        lifecycle: GuestLifecycle,
        host_baseline: HostSentinelSnapshot,
        channels: ChannelRegistry,
    ) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            lifecycle,
            host_baseline,
            channels,
            auto_retry_attempts: 0,
            saved_at: Utc::now(),
        }
    }

    pub fn save(&self, root: impl AsRef<Path>) -> HarnessResult<()> {
        fs::create_dir_all(&root).map_err(|error| {
            HarnessError::new(
                crate::error::HarnessErrorCode::Internal,
                format!("failed to create snapshot root: {error}"),
            )
        })?;
        let path = root.as_ref().join(SNAPSHOT_FILE);
        let payload = serde_json::to_vec_pretty(self).map_err(|error| {
            HarnessError::new(
                crate::error::HarnessErrorCode::Internal,
                format!("failed to encode snapshot: {error}"),
            )
        })?;
        fs::write(&path, payload).map_err(|error| {
            HarnessError::new(
                crate::error::HarnessErrorCode::Internal,
                format!("failed to write snapshot: {error}"),
            )
        })?;
        Ok(())
    }

    pub fn load(root: impl AsRef<Path>) -> HarnessResult<Self> {
        let path = root.as_ref().join(SNAPSHOT_FILE);
        let payload = fs::read(&path).map_err(|error| {
            HarnessError::new(
                crate::error::HarnessErrorCode::Internal,
                format!("failed to read snapshot: {error}"),
            )
        })?;
        let snapshot: HarnessSnapshot = serde_json::from_slice(&payload).map_err(|error| {
            HarnessError::new(
                crate::error::HarnessErrorCode::Internal,
                format!("failed to decode snapshot: {error}"),
            )
        })?;
        if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(HarnessError::invalid_state("unsupported snapshot schema"));
        }
        Ok(snapshot)
    }
}

pub fn snapshot_root(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join("isolated-surface-proof")
}
