use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionErrorCode {
    InvalidState,
    MainCheckoutProtected,
    PatchDigestMismatch,
    PatchTooLarge,
    PatchPathLimit,
    WorktreeStillActive,
    UncertainOutcome,
    AutoRetryForbidden,
    ApplyFailed,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, serde::Serialize, serde::Deserialize)]
#[error("{code:?}: {message}")]
pub struct SessionError {
    pub code: SessionErrorCode,
    pub message: String,
}

impl SessionError {
    pub fn new(code: SessionErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::InvalidState, message)
    }

    pub fn main_checkout_protected(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::MainCheckoutProtected, message)
    }

    pub fn patch_digest_mismatch(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::PatchDigestMismatch, message)
    }

    pub fn patch_too_large(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::PatchTooLarge, message)
    }

    pub fn patch_path_limit(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::PatchPathLimit, message)
    }

    pub fn worktree_still_active(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::WorktreeStillActive, message)
    }

    pub fn uncertain_outcome(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::UncertainOutcome, message)
    }

    pub fn auto_retry_forbidden(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::AutoRetryForbidden, message)
    }

    pub fn apply_failed(message: impl Into<String>) -> Self {
        Self::new(SessionErrorCode::ApplyFailed, message)
    }
}

pub type SessionResult<T> = Result<T, SessionError>;
