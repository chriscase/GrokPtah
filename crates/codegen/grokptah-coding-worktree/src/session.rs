//! `CodingWorktreeSession` — disposable worktree with reversible disposition.
//!
//! Fail-closed contract for Windowed Coding Run v0 must-have #3 (Reversible
//! code). The main checkout is never mutated without an explicit Accept to an
//! operator-chosen apply target with an exact patch digest match.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::error::{SessionError, SessionErrorCode, SessionResult};
use crate::git::{
    apply_patch, assert_worktree_removed, capture_patch, create_worktree, managed_worktree_path,
    remove_worktree, resolve_sha,
};
use crate::identity::CodingWorktreeIdentity;
use crate::lifecycle::{SessionDisposition, SessionLifecycle, SessionPhase};
use crate::patch::{digest_bytes, digest_path, PatchArtifact};
use crate::paths::assert_apply_target_allowed;
use crate::paths::resolve_worktree_write_path;
use crate::store::{snapshot_root, SessionSnapshot};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptEvidence {
    pub patch_digest: String,
    pub target_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardEvidence {
    pub worktree_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeepForReviewEvidence {
    pub worktree_path: PathBuf,
    pub patch_digest: Option<String>,
}

pub struct CodingWorktreeSession {
    identity: CodingWorktreeIdentity,
    lifecycle: SessionLifecycle,
    patch: Option<PatchArtifact>,
    repo_root: PathBuf,
    main_checkout: PathBuf,
    main_checkout_digest: String,
    worktree_path: PathBuf,
    snapshot_root: Option<PathBuf>,
    auto_retry_attempts: u32,
}

impl CodingWorktreeSession {
    /// Create a disposable worktree pinned to `base_sha`.
    ///
    /// `repo_root` is the protected main checkout. Session helpers never write
    /// to this path; only `accept` may mutate files, and only on an explicit
    /// `apply_target` that is not the main checkout.
    pub fn create(
        repo_root: impl AsRef<Path>,
        base_sha: &str,
        session_id: &str,
        now: DateTime<Utc>,
    ) -> SessionResult<Self> {
        let repo_root = repo_root.as_ref().to_path_buf();
        let main_checkout = repo_root.clone();
        let main_checkout_digest = digest_path(&main_checkout)?;
        let resolved_base = resolve_sha(&repo_root, base_sha)?;
        let branch_name = format!("grokptah/run-{session_id}");
        let worktree_path = managed_worktree_path(&repo_root, session_id);
        std::fs::create_dir_all(worktree_path.parent().expect("worktree parent")).map_err(
            |error| {
                SessionError::new(
                    SessionErrorCode::Internal,
                    format!("create worktrees root: {error}"),
                )
            },
        )?;
        create_worktree(&repo_root, &worktree_path, &resolved_base)?;
        let identity =
            CodingWorktreeIdentity::new(session_id, resolved_base, &worktree_path, branch_name)?;
        Ok(Self {
            identity,
            lifecycle: SessionLifecycle::new(now),
            patch: None,
            repo_root,
            main_checkout,
            main_checkout_digest,
            worktree_path,
            snapshot_root: None,
            auto_retry_attempts: 0,
        })
    }

    pub fn with_snapshot_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.snapshot_root = Some(root.into());
        self
    }

    pub fn identity(&self) -> &CodingWorktreeIdentity {
        &self.identity
    }

    pub fn lifecycle(&self) -> &SessionLifecycle {
        &self.lifecycle
    }

    pub fn patch(&self) -> Option<&PatchArtifact> {
        self.patch.as_ref()
    }

    pub fn worktree_path(&self) -> &Path {
        &self.worktree_path
    }

    pub fn main_checkout(&self) -> &Path {
        &self.main_checkout
    }

    pub fn auto_retry_attempts(&self) -> u32 {
        self.auto_retry_attempts
    }

    /// Write bounded edits only inside the disposable worktree.
    pub fn write_worktree_file(
        &self,
        relative: &str,
        contents: impl AsRef<[u8]>,
    ) -> SessionResult<()> {
        if !self.lifecycle.allows_staging() {
            return Err(SessionError::invalid_state(
                "staging is fenced after settlement begins",
            ));
        }
        let path = resolve_worktree_write_path(&self.worktree_path, relative)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                SessionError::new(
                    SessionErrorCode::Internal,
                    format!("create parent dirs: {error}"),
                )
            })?;
        }
        std::fs::write(&path, contents).map_err(|error| {
            SessionError::new(
                SessionErrorCode::Internal,
                format!("write worktree file: {error}"),
            )
        })?;
        Ok(())
    }

    /// Capture the worktree diff vs base as a reviewable patch artifact.
    pub fn stage_patch(&mut self) -> SessionResult<PatchArtifact> {
        if !self.lifecycle.allows_staging() {
            return Err(SessionError::invalid_state(
                "stage_patch requires Active unfenced lifecycle",
            ));
        }
        let artifact = capture_patch(&self.worktree_path, &self.identity.base_sha)?;
        self.patch = Some(artifact.clone());
        self.persist_snapshot()?;
        Ok(artifact)
    }

    /// Accept: apply the staged patch to `apply_target` after digest verification.
    ///
    /// `apply_target` must not be the protected main checkout. This is the only
    /// code path that mutates files outside the disposable worktree.
    pub fn accept(
        &mut self,
        apply_target: &Path,
        expected_patch_digest: &str,
        now: DateTime<Utc>,
    ) -> SessionResult<AcceptEvidence> {
        self.enforce_no_auto_retry("accept")?;
        assert_apply_target_allowed(&self.main_checkout, apply_target, "accept")?;
        let patch = self
            .patch
            .as_ref()
            .ok_or_else(|| SessionError::invalid_state("accept requires staged patch"))?
            .clone();
        patch.verify_digest(expected_patch_digest)?;

        self.lifecycle.begin_settlement(now)?;
        self.lifecycle.mark_apply_in_flight(now)?;
        self.persist_snapshot()?;

        if let Err(error) = apply_patch(apply_target, &patch, expected_patch_digest) {
            self.lifecycle.mark_apply_uncertain(now)?;
            self.remove_worktree_best_effort()?;
            self.persist_snapshot()?;
            return Err(error);
        }

        self.lifecycle
            .complete_settlement(SessionDisposition::Accepted, now)?;
        self.remove_worktree_best_effort()?;
        self.persist_snapshot()?;
        Ok(AcceptEvidence {
            patch_digest: digest_bytes(&patch.bytes),
            target_root: apply_target.to_path_buf(),
        })
    }

    /// Discard: remove worktree and session records without applying.
    pub fn discard(&mut self, now: DateTime<Utc>) -> SessionResult<DiscardEvidence> {
        self.enforce_no_auto_retry("discard")?;
        if self.lifecycle.phase == SessionPhase::Settled {
            return Err(SessionError::invalid_state("session already settled"));
        }
        self.lifecycle.begin_settlement(now)?;
        self.remove_worktree_best_effort()?;
        assert_worktree_removed(&self.repo_root, &self.worktree_path)?;
        self.lifecycle
            .complete_settlement(SessionDisposition::Discarded, now)?;
        self.patch = None;
        self.persist_snapshot()?;
        Ok(DiscardEvidence {
            worktree_removed: true,
        })
    }

    /// Keep for review: retain worktree and staged diff; no apply.
    pub fn keep_for_review(&mut self, now: DateTime<Utc>) -> SessionResult<KeepForReviewEvidence> {
        self.enforce_no_auto_retry("keep_for_review")?;
        if self.patch.is_none() {
            return Err(SessionError::invalid_state(
                "keep_for_review requires staged patch",
            ));
        }
        self.lifecycle.begin_settlement(now)?;
        self.lifecycle
            .complete_settlement(SessionDisposition::KeptForReview, now)?;
        self.persist_snapshot()?;
        Ok(KeepForReviewEvidence {
            worktree_path: self.worktree_path.clone(),
            patch_digest: self.patch.as_ref().map(|p| p.digest.clone()),
        })
    }

    /// Restart recovery from durable snapshot. Never auto-Accept.
    pub fn recover_after_restart(&mut self, now: DateTime<Utc>) -> SessionResult<()> {
        self.lifecycle.recover_after_restart(now)?;
        if self.lifecycle.disposition == Some(SessionDisposition::Uncertain) {
            self.remove_worktree_best_effort()?;
        } else if self.lifecycle.disposition == Some(SessionDisposition::Discarded)
            || self.lifecycle.disposition == Some(SessionDisposition::Accepted)
        {
            self.remove_worktree_best_effort()?;
            assert_worktree_removed(&self.repo_root, &self.worktree_path)?;
        }
        self.persist_snapshot()?;
        Ok(())
    }

    pub fn restore_from_snapshot(snapshot: SessionSnapshot) -> SessionResult<Self> {
        let mut lifecycle = snapshot.lifecycle;
        lifecycle.reconcile_invariants()?;
        let repo_root = snapshot.repo_root.clone();
        Ok(Self {
            identity: snapshot.identity,
            lifecycle,
            patch: snapshot.patch,
            repo_root: repo_root.clone(),
            main_checkout: repo_root,
            main_checkout_digest: snapshot.main_checkout_digest,
            worktree_path: snapshot.worktree_path,
            snapshot_root: None,
            auto_retry_attempts: snapshot.auto_retry_attempts,
        })
    }

    pub fn to_snapshot(&self) -> SessionSnapshot {
        SessionSnapshot::new(
            self.identity.clone(),
            self.lifecycle.clone(),
            self.patch.clone(),
            self.main_checkout_digest.clone(),
            self.worktree_path.clone(),
            self.repo_root.clone(),
        )
    }

    fn remove_worktree_best_effort(&mut self) -> SessionResult<()> {
        remove_worktree(&self.repo_root, &self.worktree_path)?;
        if self.worktree_path.exists() {
            std::fs::remove_dir_all(&self.worktree_path).map_err(|error| {
                SessionError::new(
                    SessionErrorCode::Internal,
                    format!("remove worktree directory: {error}"),
                )
            })?;
        }
        Ok(())
    }

    fn enforce_no_auto_retry(&self, op: &str) -> SessionResult<()> {
        if self.lifecycle.disposition == Some(SessionDisposition::Uncertain) {
            return Err(SessionError::uncertain_outcome(format!(
                "{op} forbidden while disposition is Uncertain"
            )));
        }
        if self.auto_retry_attempts > 0 {
            return Err(SessionError::auto_retry_forbidden(format!(
                "{op} forbidden after auto-retry was attempted"
            )));
        }
        Ok(())
    }

    fn persist_snapshot(&self) -> SessionResult<()> {
        if let Some(root) = &self.snapshot_root {
            let snapshot = self.to_snapshot();
            snapshot.save(snapshot_root(root))?;
        }
        Ok(())
    }
}

/// Human-readable non-claim for harness evidence.
pub const SYNTHETIC_SESSION_NONCLAIM: &str =
    "Synthetic temp-dir harness evidence does not mutate the developer's real GrokPtah checkout.";
