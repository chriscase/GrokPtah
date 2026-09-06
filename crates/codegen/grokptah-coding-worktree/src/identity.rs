//! Session identity recorded at creation time.

use serde::{Deserialize, Serialize};

use crate::error::SessionResult;
use crate::patch::digest_path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingWorktreeIdentity {
    pub session_id: String,
    pub base_sha: String,
    pub worktree_path_digest: String,
    pub branch_name: String,
}

impl CodingWorktreeIdentity {
    pub fn new(
        session_id: impl Into<String>,
        base_sha: impl Into<String>,
        worktree_path: &std::path::Path,
        branch_name: impl Into<String>,
    ) -> SessionResult<Self> {
        Ok(Self {
            session_id: session_id.into(),
            base_sha: base_sha.into(),
            worktree_path_digest: digest_path(worktree_path)?,
            branch_name: branch_name.into(),
        })
    }
}
