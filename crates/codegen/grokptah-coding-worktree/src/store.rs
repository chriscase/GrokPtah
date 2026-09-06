//! Durable snapshot for restart recovery tests.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{SessionError, SessionErrorCode, SessionResult};
use crate::identity::CodingWorktreeIdentity;
use crate::lifecycle::SessionLifecycle;
use crate::patch::PatchArtifact;

pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub const SNAPSHOT_FILE: &str = "coding_worktree_session.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub schema_version: u32,
    pub identity: CodingWorktreeIdentity,
    pub lifecycle: SessionLifecycle,
    pub patch: Option<PatchArtifact>,
    pub main_checkout_digest: String,
    pub worktree_path: PathBuf,
    pub repo_root: PathBuf,
    pub auto_retry_attempts: u32,
    pub saved_at: DateTime<Utc>,
}

impl SessionSnapshot {
    pub fn new(
        identity: CodingWorktreeIdentity,
        lifecycle: SessionLifecycle,
        patch: Option<PatchArtifact>,
        main_checkout_digest: String,
        worktree_path: PathBuf,
        repo_root: PathBuf,
    ) -> Self {
        Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            identity,
            lifecycle,
            patch,
            main_checkout_digest,
            worktree_path,
            repo_root,
            auto_retry_attempts: 0,
            saved_at: Utc::now(),
        }
    }

    pub fn save(&self, root: impl AsRef<Path>) -> SessionResult<()> {
        fs::create_dir_all(root.as_ref()).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("failed to create snapshot root: {error}"),
            )
        })?;
        let path = root.as_ref().join(SNAPSHOT_FILE);
        let payload = serde_json::to_vec_pretty(self).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("failed to encode snapshot: {error}"),
            )
        })?;
        fs::write(&path, payload).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("failed to write snapshot: {error}"),
            )
        })?;
        Ok(())
    }

    pub fn load(root: impl AsRef<Path>) -> SessionResult<Self> {
        let path = root.as_ref().join(SNAPSHOT_FILE);
        let payload = fs::read(&path).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("failed to read snapshot: {error}"),
            )
        })?;
        let snapshot: SessionSnapshot = serde_json::from_slice(&payload).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("failed to decode snapshot: {error}"),
            )
        })?;
        if snapshot.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(SessionError::invalid_state("unsupported snapshot schema"));
        }
        Ok(snapshot)
    }
}

pub fn snapshot_root(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join("coding-worktree-session")
}
