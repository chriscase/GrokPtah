//! Reversible coding worktree session contract for Windowed Coding Run v0.
//!
//! Durable Accept / Discard / Keep-for-review disposition for a disposable
//! coding worktree. Fail-closed so the main checkout is never mutated without
//! an explicit Accept to a non-main apply target with an exact patch digest.
//!
//! Related issues: [#288](https://github.com/chriscase/GrokPtah/issues/288),
//! [#286](https://github.com/chriscase/GrokPtah/issues/286),
//! [#267](https://github.com/chriscase/GrokPtah/issues/267).

mod error;
mod git;
mod identity;
mod lifecycle;
mod patch;
mod session;
mod store;

pub use error::{SessionError, SessionErrorCode, SessionResult};
pub use identity::CodingWorktreeIdentity;
pub use lifecycle::{SessionDisposition, SessionLifecycle, SessionPhase, LIFECYCLE_SCHEMA_VERSION};
pub use patch::{digest_bytes, digest_path, PatchArtifact, MAX_PATCH_BYTES, MAX_PATCH_PATHS};
pub use session::{
    AcceptEvidence, CodingWorktreeSession, DiscardEvidence, KeepForReviewEvidence,
    SYNTHETIC_SESSION_NONCLAIM,
};
pub use store::{snapshot_root, SessionSnapshot, SNAPSHOT_FILE, SNAPSHOT_SCHEMA_VERSION};
