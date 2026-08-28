use serde::{Deserialize, Serialize};

/// Closed error codes aligned with `ComputerErrorCode` in grokptah-agent-bridge.
/// Isolated visual never invents a second public error taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedErrorCode {
    InvalidRequest,
    InvalidState,
    Unauthorized,
    PermissionRequired,
    PermissionDenied,
    ForbiddenTarget,
    ForbiddenAction,
    StaleObservation,
    TargetChanged,
    LimitReached,
    Conflict,
    Pending,
    UncertainOutcome,
    Interrupted,
    BackendUnavailable,
    BackendFailure,
    UnsupportedPlatform,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct IsolatedError {
    pub code: IsolatedErrorCode,
    pub message: String,
}

impl IsolatedError {
    pub fn new(code: IsolatedErrorCode, message: impl Into<String>) -> Self {
        let mut message = message.into();
        if message.len() > 512 {
            message.truncate(512);
        }
        message.retain(|ch| !ch.is_control());
        Self { code, message }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::InvalidRequest, message)
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::InvalidState, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::ForbiddenAction, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::Conflict, message)
    }

    pub fn stale(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::StaleObservation, message)
    }

    pub fn limit(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::LimitReached, message)
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::UnsupportedPlatform, message)
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::BackendUnavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::Internal, message)
    }

    pub fn interrupted(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::Interrupted, message)
    }

    pub fn uncertain(message: impl Into<String>) -> Self {
        Self::new(IsolatedErrorCode::UncertainOutcome, message)
    }
}

pub type IsolatedResult<T> = Result<T, IsolatedError>;
