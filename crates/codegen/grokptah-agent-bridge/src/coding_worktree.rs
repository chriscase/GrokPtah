//! Fail-closed bridge seam for the Coding Worktree Session contract (#288/#286/#267).
//!
//! Windowed Coding Run v0 must-have #3 (Reversible code). Admission and
//! Computer Mode remain unchanged; this module exposes the session contract
//! and synthetic harness only.

pub use grokptah_coding_worktree::{
    digest_bytes, digest_path, snapshot_root, AcceptEvidence, CodingWorktreeIdentity,
    CodingWorktreeSession, DiscardEvidence, KeepForReviewEvidence, PatchArtifact,
    SessionDisposition, SessionError, SessionErrorCode, SessionLifecycle, SessionPhase,
    SessionResult, SessionSnapshot, LIFECYCLE_SCHEMA_VERSION, MAX_PATCH_BYTES, MAX_PATCH_PATHS,
    SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION, SYNTHETIC_SESSION_NONCLAIM,
};
