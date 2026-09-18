//! Fail-closed AgentHost seam for the Coding Worktree Session contract
//! (#288/#286/#267).
//!
//! Windowed Coding Run v0 must-have #3 (Reversible code). Admission and
//! Computer Mode remain unchanged: this module owns/attaches a
//! [`CodingWorktreeSession`] per explicit handle (and optionally per AgentHost
//! session) and exposes Pause / fence-first Stop / Accept / Discard /
//! Keep-for-review. It does not flip `isolated_surface_admission_available()`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use uuid::Uuid;

use crate::host::AgentHostHandle;

pub use grokptah_coding_worktree::{
    assert_apply_target_allowed, digest_bytes, digest_path, snapshot_root, AcceptEvidence,
    CodingWorktreeIdentity, CodingWorktreeSession, DiscardEvidence, KeepForReviewEvidence,
    PatchArtifact, PauseEvidence, SessionDisposition, SessionError, SessionErrorCode,
    SessionLifecycle, SessionPhase, SessionResult, SessionSnapshot, StopEvidence,
    LIFECYCLE_SCHEMA_VERSION, MAX_PATCH_BYTES, MAX_PATCH_PATHS, SNAPSHOT_FILE,
    SNAPSHOT_SCHEMA_VERSION, SYNTHETIC_SESSION_NONCLAIM,
};

/// In-memory coding-worktree map owned by [`AgentHostHandle`].
#[derive(Default)]
pub(crate) struct CodingWorktreeRegistry {
    by_handle: HashMap<String, CodingWorktreeSlot>,
    by_agent_session: HashMap<Uuid, String>,
}

struct CodingWorktreeSlot {
    session: CodingWorktreeSession,
    agent_session_id: Option<Uuid>,
}

/// Read-only projection of a host-owned coding worktree session.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingWorktreeHostView {
    pub handle: String,
    pub agent_session_id: Option<Uuid>,
    pub identity: CodingWorktreeIdentity,
    pub phase: SessionPhase,
    pub disposition: Option<SessionDisposition>,
    pub pause_fenced: bool,
    pub settlement_fenced: bool,
    pub apply_uncertain: bool,
    pub worktree_path: PathBuf,
    pub patch_digest: Option<String>,
}

impl AgentHostHandle {
    /// Create a disposable coding worktree and attach it to this host.
    ///
    /// `handle` is the explicit coding-worktree session id. When
    /// `agent_session_id` is set, the AgentHost session must exist and must not
    /// already own another coding worktree.
    pub fn coding_worktree_create(
        &self,
        agent_session_id: Option<Uuid>,
        repo_root: impl AsRef<Path>,
        base_sha: &str,
        handle: &str,
    ) -> SessionResult<CodingWorktreeIdentity> {
        self.ensure_coding_worktree_open("creating a coding worktree session")?;
        validate_coding_worktree_handle(handle)?;
        self.assert_agent_session_bindable(agent_session_id)?;
        self.assert_coding_worktree_available(handle, agent_session_id)?;

        let session = CodingWorktreeSession::create(repo_root, base_sha, handle, Utc::now())?
            .with_snapshot_root(self.coding_worktree_snapshot_root(handle));
        session.persist_create_metadata()?;
        let identity = session.identity().clone();
        self.insert_coding_worktree(handle, agent_session_id, session)?;
        Ok(identity)
    }

    /// Attach a previously persisted coding worktree from durable snapshot
    /// metadata. Settled and Uncertain restorations stay fenced: Pause and
    /// settlement cannot be retried.
    pub fn coding_worktree_attach(
        &self,
        agent_session_id: Option<Uuid>,
        snapshot_base: impl AsRef<Path>,
    ) -> SessionResult<CodingWorktreeIdentity> {
        self.ensure_coding_worktree_open("attaching a coding worktree session")?;
        self.assert_agent_session_bindable(agent_session_id)?;

        let session = CodingWorktreeSession::attach(snapshot_base)?;
        let identity = session.identity().clone();
        validate_coding_worktree_handle(&identity.session_id)?;
        self.assert_coding_worktree_available(&identity.session_id, agent_session_id)?;
        self.insert_coding_worktree(&identity.session_id, agent_session_id, session)?;
        Ok(identity)
    }

    /// Drop the in-memory session without teardown so a later
    /// [`Self::coding_worktree_attach`] reloads durable snapshot metadata.
    /// Does not destroy the worktree and is not a disposition.
    pub fn coding_worktree_detach_live(
        &self,
        handle: &str,
    ) -> SessionResult<CodingWorktreeIdentity> {
        self.ensure_coding_worktree_open("detaching a live coding worktree session")?;
        let mut registry = self.coding_worktrees.lock();
        let slot = registry.by_handle.remove(handle).ok_or_else(|| {
            SessionError::invalid_state(format!(
                "no coding worktree session attached for handle {handle}"
            ))
        })?;
        if let Some(agent_session_id) = slot.agent_session_id {
            registry.by_agent_session.remove(&agent_session_id);
        }
        Ok(slot.session.identity().clone())
    }

    pub fn coding_worktree_view(&self, handle: &str) -> SessionResult<CodingWorktreeHostView> {
        self.with_coding_worktree(handle, |slot| Ok(slot.view(handle)))
    }

    pub fn coding_worktree_handle_for_session(
        &self,
        agent_session_id: Uuid,
    ) -> SessionResult<String> {
        let registry = self.coding_worktrees.lock();
        registry
            .by_agent_session
            .get(&agent_session_id)
            .cloned()
            .ok_or_else(|| {
                SessionError::invalid_state(format!(
                    "no coding worktree session attached for agent session {agent_session_id}"
                ))
            })
    }

    pub fn coding_worktree_write_file(
        &self,
        handle: &str,
        relative: &str,
        contents: impl AsRef<[u8]>,
    ) -> SessionResult<()> {
        self.ensure_coding_worktree_open("writing a coding worktree file")?;
        self.with_coding_worktree_mut(handle, |slot| {
            slot.session.write_worktree_file(relative, contents)
        })
    }

    pub fn coding_worktree_stage_patch(&self, handle: &str) -> SessionResult<PatchArtifact> {
        self.ensure_coding_worktree_open("staging a coding worktree patch")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.stage_patch())
    }

    /// Local Pause: fences further staging and settlement. Stop remains legal.
    pub fn coding_worktree_pause(&self, handle: &str) -> SessionResult<PauseEvidence> {
        self.ensure_coding_worktree_open("pausing a coding worktree session")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.pause(Utc::now()))
    }

    /// Fence-first Stop. `Destroyed` is recorded only when worktree destroy is
    /// confirmed; persist/snapshot failure does not skip teardown.
    pub fn coding_worktree_stop(&self, handle: &str) -> SessionResult<StopEvidence> {
        self.ensure_coding_worktree_open("stopping a coding worktree session")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.stop(Utc::now()))
    }

    /// Stop the coding worktree bound to an AgentHost session, if any.
    pub fn coding_worktree_stop_for_agent_session(
        &self,
        agent_session_id: Uuid,
    ) -> SessionResult<Option<StopEvidence>> {
        let handle = {
            let registry = self.coding_worktrees.lock();
            registry.by_agent_session.get(&agent_session_id).cloned()
        };
        match handle {
            Some(handle) => self.coding_worktree_stop(&handle).map(Some),
            None => Ok(None),
        }
    }

    /// Accept: apply the staged patch to `apply_target` after an exact
    /// `sha256:` digest match. The protected main checkout, host project cwd,
    /// and bound AgentHost session cwd are never apply targets.
    pub fn coding_worktree_accept(
        &self,
        handle: &str,
        apply_target: impl AsRef<Path>,
        expected_patch_digest: &str,
    ) -> SessionResult<AcceptEvidence> {
        self.ensure_coding_worktree_open("accepting a coding worktree session")?;
        let apply_target = apply_target.as_ref();
        let protected = self.coding_worktree_protected_checkouts(handle)?;
        for root in &protected {
            assert_apply_target_allowed(root, apply_target, "accept")?;
        }
        self.with_coding_worktree_mut(handle, |slot| {
            slot.session
                .accept(apply_target, expected_patch_digest, Utc::now())
        })
    }

    /// Discard: remove the disposable worktree and clear session records.
    pub fn coding_worktree_discard(&self, handle: &str) -> SessionResult<DiscardEvidence> {
        self.ensure_coding_worktree_open("discarding a coding worktree session")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.discard(Utc::now()))
    }

    /// Keep for review: retain worktree + staged diff; no apply.
    pub fn coding_worktree_keep_for_review(
        &self,
        handle: &str,
    ) -> SessionResult<KeepForReviewEvidence> {
        self.ensure_coding_worktree_open("keeping a coding worktree session for review")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.keep_for_review(Utc::now()))
    }

    /// Test hook matching [`CodingWorktreeSession::fail_next_destroy_for_test`].
    pub fn coding_worktree_fail_next_destroy_for_test(&self, handle: &str) -> SessionResult<()> {
        self.with_coding_worktree_mut(handle, |slot| {
            slot.session.fail_next_destroy_for_test();
            Ok(())
        })
    }

    fn coding_worktree_snapshot_root(&self, handle: &str) -> PathBuf {
        self.runtime_home()
            .path()
            .join("coding-worktrees")
            .join(handle)
    }

    fn assert_coding_worktree_available(
        &self,
        handle: &str,
        agent_session_id: Option<Uuid>,
    ) -> SessionResult<()> {
        let registry = self.coding_worktrees.lock();
        if registry.by_handle.contains_key(handle) {
            return Err(SessionError::invalid_state(format!(
                "coding worktree session {handle} is already attached"
            )));
        }
        if let Some(agent_session_id) = agent_session_id {
            if let Some(existing) = registry.by_agent_session.get(&agent_session_id) {
                return Err(SessionError::invalid_state(format!(
                    "AgentHost session {agent_session_id} already owns coding worktree {existing}"
                )));
            }
        }
        Ok(())
    }

    fn ensure_coding_worktree_open(&self, operation: &str) -> SessionResult<()> {
        self.ensure_accepting(operation)
            .map_err(|error| SessionError::invalid_state(error.to_string()))
    }

    fn assert_agent_session_bindable(&self, agent_session_id: Option<Uuid>) -> SessionResult<()> {
        let Some(agent_session_id) = agent_session_id else {
            return Ok(());
        };
        let inner = self.inner.lock();
        if !inner.sessions.contains_key(&agent_session_id) {
            return Err(SessionError::invalid_state(format!(
                "unknown AgentHost session {agent_session_id}"
            )));
        }
        Ok(())
    }

    fn insert_coding_worktree(
        &self,
        handle: &str,
        agent_session_id: Option<Uuid>,
        session: CodingWorktreeSession,
    ) -> SessionResult<()> {
        let mut registry = self.coding_worktrees.lock();
        if registry.by_handle.contains_key(handle) {
            return Err(SessionError::invalid_state(format!(
                "coding worktree session {handle} is already attached"
            )));
        }
        if let Some(agent_session_id) = agent_session_id {
            if let Some(existing) = registry.by_agent_session.get(&agent_session_id) {
                return Err(SessionError::invalid_state(format!(
                    "AgentHost session {agent_session_id} already owns coding worktree {existing}"
                )));
            }
            registry
                .by_agent_session
                .insert(agent_session_id, handle.to_string());
        }
        registry.by_handle.insert(
            handle.to_string(),
            CodingWorktreeSlot {
                session,
                agent_session_id,
            },
        );
        Ok(())
    }

    fn with_coding_worktree<T>(
        &self,
        handle: &str,
        f: impl FnOnce(&CodingWorktreeSlot) -> SessionResult<T>,
    ) -> SessionResult<T> {
        let registry = self.coding_worktrees.lock();
        let slot = registry.by_handle.get(handle).ok_or_else(|| {
            SessionError::invalid_state(format!(
                "no coding worktree session attached for handle {handle}"
            ))
        })?;
        f(slot)
    }

    fn with_coding_worktree_mut<T>(
        &self,
        handle: &str,
        f: impl FnOnce(&mut CodingWorktreeSlot) -> SessionResult<T>,
    ) -> SessionResult<T> {
        let mut registry = self.coding_worktrees.lock();
        let slot = registry.by_handle.get_mut(handle).ok_or_else(|| {
            SessionError::invalid_state(format!(
                "no coding worktree session attached for handle {handle}"
            ))
        })?;
        f(slot)
    }

    fn coding_worktree_protected_checkouts(&self, handle: &str) -> SessionResult<Vec<PathBuf>> {
        let (main_checkout, agent_session_id) = self.with_coding_worktree(handle, |slot| {
            Ok((
                slot.session.main_checkout().to_path_buf(),
                slot.agent_session_id,
            ))
        })?;
        let mut roots = Vec::new();
        if main_checkout.exists() {
            roots.push(main_checkout);
        }
        let inner = self.inner.lock();
        if let Some(cwd) = inner.project_cwd.as_ref() {
            if cwd.exists() {
                roots.push(cwd.clone());
            }
        }
        if let Some(agent_session_id) = agent_session_id {
            if let Some(session) = inner.sessions.get(&agent_session_id) {
                if session.cwd.exists() {
                    roots.push(session.cwd.clone());
                }
            }
        }
        Ok(roots)
    }
}

impl CodingWorktreeSlot {
    fn view(&self, handle: &str) -> CodingWorktreeHostView {
        let lifecycle = self.session.lifecycle();
        CodingWorktreeHostView {
            handle: handle.to_string(),
            agent_session_id: self.agent_session_id,
            identity: self.session.identity().clone(),
            phase: lifecycle.phase,
            disposition: lifecycle.disposition,
            pause_fenced: lifecycle.pause_fenced,
            settlement_fenced: lifecycle.settlement_fenced,
            apply_uncertain: lifecycle.apply_uncertain,
            worktree_path: self.session.worktree_path().to_path_buf(),
            patch_digest: self.session.patch().map(|patch| patch.digest.clone()),
        }
    }
}

fn validate_coding_worktree_handle(handle: &str) -> SessionResult<()> {
    if handle.is_empty() {
        return Err(SessionError::invalid_state(
            "coding worktree session id is required",
        ));
    }
    if handle.contains('/')
        || handle.contains('\\')
        || handle.contains('\0')
        || handle == "."
        || handle == ".."
        || handle.contains("..")
    {
        return Err(SessionError::invalid_state(
            "coding worktree session id must be a single path segment",
        ));
    }
    Ok(())
}
