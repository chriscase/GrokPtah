use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessErrorCode {
    InvalidState,
    InjectFenced,
    UncertainOutcome,
    HostSentinelViolation,
    ChannelLeak,
    AutoRetryForbidden,
    BackendUnavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, serde::Serialize, serde::Deserialize)]
#[error("{code:?}: {message}")]
pub struct HarnessError {
    pub code: HarnessErrorCode,
    pub message: String,
}

impl HarnessError {
    pub fn new(code: HarnessErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::InvalidState, message)
    }

    pub fn inject_fenced(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::InjectFenced, message)
    }

    pub fn uncertain_outcome(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::UncertainOutcome, message)
    }

    pub fn host_sentinel_violation(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::HostSentinelViolation, message)
    }

    pub fn channel_leak(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::ChannelLeak, message)
    }

    pub fn auto_retry_forbidden(message: impl Into<String>) -> Self {
        Self::new(HarnessErrorCode::AutoRetryForbidden, message)
    }
}

pub type HarnessResult<T> = Result<T, HarnessError>;
