//! Selectors for scoped run reads. Every run read is session+workspace+run.

use crate::ids::{RunId, SessionId, WorkspaceRef};

/// Identity required by `ptah_get_run` and `ptah_get_events`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSelector {
    pub(crate) session_id: SessionId,
    pub(crate) workspace: WorkspaceRef,
    pub(crate) run_id: RunId,
}

impl RunSelector {
    pub fn new(session_id: SessionId, workspace: WorkspaceRef, run_id: RunId) -> Self {
        Self {
            session_id,
            workspace,
            run_id,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn workspace(&self) -> &WorkspaceRef {
        &self.workspace
    }

    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }
}

/// Session+workspace scope for `ptah_list_runs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionScope {
    pub(crate) session_id: SessionId,
    pub(crate) workspace: WorkspaceRef,
}

impl SessionScope {
    pub fn new(session_id: SessionId, workspace: WorkspaceRef) -> Self {
        Self {
            session_id,
            workspace,
        }
    }

    pub fn from_selector(selector: &RunSelector) -> Self {
        Self {
            session_id: selector.session_id.clone(),
            workspace: selector.workspace.clone(),
        }
    }
}

/// Optional `ptah_get_events` page controls. `limit` is validated against 1..=500.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EventQuery {
    pub after_seq: Option<u64>,
    pub limit: Option<u32>,
}
