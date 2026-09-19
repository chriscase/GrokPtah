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
        let _write = self.ensure_coding_worktree_write("creating a coding worktree session")?;
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
    /// settlement cannot be retried. Apply-in-flight (including hostile
    /// Active+in-flight snapshots) recovers to Uncertain and cannot auto-retry.
    pub fn coding_worktree_attach(
        &self,
        agent_session_id: Option<Uuid>,
        snapshot_base: impl AsRef<Path>,
    ) -> SessionResult<CodingWorktreeIdentity> {
        let _write = self.ensure_coding_worktree_write("attaching a coding worktree session")?;
        self.assert_agent_session_bindable(agent_session_id)?;

        let mut session = CodingWorktreeSession::attach(snapshot_base)?;
        // `CodingWorktreeSession::attach` already recovers apply-in-flight.
        // Re-run here so a host attach of a pre-recovery snapshot cannot skip
        // the fence if crate attach is ever split from recover.
        if session.lifecycle().apply_in_flight
            || session.lifecycle().apply_uncertain
            || session.lifecycle().phase == SessionPhase::Settling
        {
            session.recover_after_restart(Utc::now())?;
        }
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
        let _write = self.ensure_coding_worktree_write("writing a coding worktree file")?;
        self.with_coding_worktree_mut(handle, |slot| {
            slot.session.write_worktree_file(relative, contents)
        })
    }

    pub fn coding_worktree_stage_patch(&self, handle: &str) -> SessionResult<PatchArtifact> {
        let _write = self.ensure_coding_worktree_write("staging a coding worktree patch")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.stage_patch())
    }

    /// Local Pause: fences further staging and settlement. Stop remains legal.
    pub fn coding_worktree_pause(&self, handle: &str) -> SessionResult<PauseEvidence> {
        let _write = self.ensure_coding_worktree_write("pausing a coding worktree session")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.pause(Utc::now()))
    }

    /// Fence-first Stop. `Destroyed` is recorded only when worktree destroy is
    /// confirmed; persist/snapshot failure does not skip teardown.
    pub fn coding_worktree_stop(&self, handle: &str) -> SessionResult<StopEvidence> {
        let _write = self
            .durable_write("stopping a coding worktree session")
            .ok();
        self.with_coding_worktree_mut(handle, |slot| slot.session.stop(Utc::now()))
    }

    /// Stop the coding worktree bound to an AgentHost session, if any.
    /// Already-`Destroyed` worktrees are treated as confirmed so a later
    /// `session_delete` does not fail closed on a completed Stop.
    pub fn coding_worktree_stop_for_agent_session(
        &self,
        agent_session_id: Uuid,
    ) -> SessionResult<Option<StopEvidence>> {
        let handle = {
            let registry = self.coding_worktrees.lock();
            registry.by_agent_session.get(&agent_session_id).cloned()
        };
        let Some(handle) = handle else {
            return Ok(None);
        };
        let phase =
            self.with_coding_worktree(&handle, |slot| Ok(slot.session.lifecycle().phase))?;
        if phase == SessionPhase::Destroyed {
            return self.with_coding_worktree(&handle, |slot| {
                let lifecycle = slot.session.lifecycle();
                Ok(Some(StopEvidence {
                    session_id: slot.session.identity().session_id.clone(),
                    worktree_removed: !slot.session.worktree_path().exists(),
                    destroy_confirmed: true,
                    persist_snapshot_error: None,
                    disposition: lifecycle.disposition,
                    phase: lifecycle.phase,
                }))
            });
        }
        self.coding_worktree_stop(&handle).map(Some)
    }

    /// Pause the coding worktree bound to an AgentHost session, if any.
    /// Already-fenced / Uncertain sessions stay fenced (not an archive error).
    pub fn coding_worktree_pause_for_agent_session(
        &self,
        agent_session_id: Uuid,
    ) -> SessionResult<Option<PauseEvidence>> {
        let handle = {
            let registry = self.coding_worktrees.lock();
            registry.by_agent_session.get(&agent_session_id).cloned()
        };
        match handle {
            Some(handle) => match self.coding_worktree_pause(&handle) {
                Ok(evidence) => Ok(Some(evidence)),
                Err(error)
                    if matches!(
                        error.code,
                        SessionErrorCode::InvalidState | SessionErrorCode::UncertainOutcome
                    ) =>
                {
                    Ok(None)
                }
                Err(error) => Err(error),
            },
            None => Ok(None),
        }
    }

    /// Drop the AgentHost session binding after a confirmed Stop. The live slot
    /// remains so Destroyed/Stopped can still be inspected.
    pub fn coding_worktree_unbind_agent_session(&self, agent_session_id: Uuid) {
        let mut registry = self.coding_worktrees.lock();
        if let Some(handle) = registry.by_agent_session.remove(&agent_session_id) {
            if let Some(slot) = registry.by_handle.get_mut(&handle) {
                slot.agent_session_id = None;
            }
        }
    }

    /// Fence-first Stop every attached coding worktree. Persist failure does
    /// not skip teardown. `Destroyed` only if destroy is confirmed per session.
    pub fn coding_worktree_stop_all(&self) {
        let handles: Vec<String> = {
            let registry = self.coding_worktrees.lock();
            registry.by_handle.keys().cloned().collect()
        };
        for handle in handles {
            let _ = self.coding_worktree_stop(&handle);
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
        let _write = self.ensure_coding_worktree_write("accepting a coding worktree session")?;
        let apply_target = apply_target.as_ref();
        let mut registry = self.coding_worktrees.lock();
        let slot = registry.by_handle.get_mut(handle).ok_or_else(|| {
            SessionError::invalid_state(format!(
                "no coding worktree session attached for handle {handle}"
            ))
        })?;
        // Hold Inner for the duration of apply so set_project_cwd /
        // session_set_cwd cannot swap a host-protected root under Accept.
        let inner = self.inner.lock();
        let mut roots = Vec::new();
        if slot.session.main_checkout().exists() {
            roots.push(slot.session.main_checkout().to_path_buf());
        }
        if let Some(cwd) = inner.project_cwd.as_ref() {
            if cwd.exists() {
                roots.push(cwd.clone());
            }
        }
        if let Some(agent_session_id) = slot.agent_session_id {
            if let Some(session) = inner.sessions.get(&agent_session_id) {
                if session.cwd.exists() {
                    roots.push(session.cwd.clone());
                }
            }
        }
        for root in &roots {
            assert_apply_target_allowed(root, apply_target, "accept")?;
        }
        let evidence = slot
            .session
            .accept(apply_target, expected_patch_digest, Utc::now())?;
        drop(inner);
        Ok(evidence)
    }

    /// Discard: remove the disposable worktree and clear session records.
    pub fn coding_worktree_discard(&self, handle: &str) -> SessionResult<DiscardEvidence> {
        let _write = self.ensure_coding_worktree_write("discarding a coding worktree session")?;
        self.with_coding_worktree_mut(handle, |slot| slot.session.discard(Utc::now()))
    }

    /// Keep for review: retain worktree + staged diff; no apply.
    pub fn coding_worktree_keep_for_review(
        &self,
        handle: &str,
    ) -> SessionResult<KeepForReviewEvidence> {
        let _write =
            self.ensure_coding_worktree_write("keeping a coding worktree session for review")?;
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

    fn ensure_coding_worktree_write(
        &self,
        operation: &str,
    ) -> SessionResult<crate::host_runtime::DurableWriteGuard> {
        self.durable_write(operation)
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
