//! Durable provider-dispatch attempt records.
//!
//! A row is installed before the host enters the transport. If the process
//! disappears while it is admitted, restart conservatively treats the send as
//! potentially accepted. Only an outcome proven not to have put request bytes
//! on the wire is safe for same-Run retry.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::completion::CompletionUsage;

use super::types::{OrchError, OrchErrorCode, RunRecord};

pub const PROVIDER_ATTEMPT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderSendCertainty {
    KnownNotSent,
    KnownAccepted,
    UncertainAccept,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderRetryClass {
    SameRunSafe,
    ExplicitNewRunOnly,
}

impl ProviderSendCertainty {
    pub fn retry_class(self) -> ProviderRetryClass {
        match self {
            Self::KnownNotSent => ProviderRetryClass::SameRunSafe,
            Self::KnownAccepted | Self::UncertainAccept => ProviderRetryClass::ExplicitNewRunOnly,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptState {
    Admitted,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttemptRecord {
    pub schema_version: u32,
    pub attempt_id: String,
    pub run_id: String,
    pub reservation_id: String,
    pub route_snapshot_hash: String,
    pub ordinal: u64,
    pub state: ProviderAttemptState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_certainty: Option<ProviderSendCertainty>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_class: Option<ProviderRetryClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<CompletionUsage>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

impl ProviderAttemptRecord {
    pub fn admitted(
        run: &RunRecord,
        attempt_id: impl Into<String>,
        ordinal: u64,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        let route = run.provider_route.as_ref().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt requires a provider route snapshot",
            )
        })?;
        route.validate()?;
        let reservation_id = route.quota_reservation_id.clone().ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt requires a quota reservation",
            )
        })?;
        let attempt = Self {
            schema_version: PROVIDER_ATTEMPT_SCHEMA_VERSION,
            attempt_id: attempt_id.into(),
            run_id: run.run_id.clone(),
            reservation_id,
            route_snapshot_hash: route.snapshot_hash.clone(),
            ordinal,
            state: ProviderAttemptState::Admitted,
            send_certainty: None,
            retry_class: None,
            http_status: None,
            usage: None,
            created_at: now,
            updated_at: now,
            finished_at: None,
        };
        attempt.validate()?;
        Ok(attempt)
    }

    pub fn finish(
        &mut self,
        certainty: ProviderSendCertainty,
        http_status: Option<u16>,
        usage: Option<CompletionUsage>,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        if self.state == ProviderAttemptState::Finished {
            if self.send_certainty == Some(certainty)
                && self.http_status == http_status
                && self.usage == usage
            {
                return Ok(());
            }
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "provider attempt completion conflicts with durable state",
            ));
        }
        if certainty != ProviderSendCertainty::KnownAccepted && usage.is_some() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider usage requires a known accepted response",
            ));
        }
        if certainty == ProviderSendCertainty::KnownNotSent && http_status.is_some() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "a known-not-sent attempt cannot have an HTTP status",
            ));
        }
        self.state = ProviderAttemptState::Finished;
        self.send_certainty = Some(certainty);
        self.retry_class = Some(certainty.retry_class());
        self.http_status = http_status;
        self.usage = usage;
        self.updated_at = self.updated_at.max(now);
        self.finished_at = Some(self.updated_at);
        self.validate()
    }

    pub fn validate(&self) -> Result<(), OrchError> {
        if self.schema_version != PROVIDER_ATTEMPT_SCHEMA_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt schema is invalid",
            ));
        }
        for (value, field) in [
            (self.attempt_id.as_str(), "attempt_id"),
            (self.run_id.as_str(), "run_id"),
            (self.reservation_id.as_str(), "reservation_id"),
            (self.route_snapshot_hash.as_str(), "route_snapshot_hash"),
        ] {
            if value.is_empty() || value.len() > 2_048 || value.contains('\0') {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("provider attempt {field} is invalid"),
                ));
            }
        }
        if self.ordinal == 0 || self.updated_at < self.created_at {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt ordinal or timestamps are invalid",
            ));
        }
        match self.state {
            ProviderAttemptState::Admitted
                if self.send_certainty.is_none()
                    && self.retry_class.is_none()
                    && self.http_status.is_none()
                    && self.usage.is_none()
                    && self.finished_at.is_none() => {}
            ProviderAttemptState::Finished => {
                let certainty = self.send_certainty.ok_or_else(|| {
                    OrchError::new(
                        OrchErrorCode::Conflict,
                        "finished provider attempt has no send certainty",
                    )
                })?;
                if self.retry_class != Some(certainty.retry_class())
                    || self.finished_at != Some(self.updated_at)
                {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "provider attempt retry class or completion timestamp is invalid",
                    ));
                }
                if certainty != ProviderSendCertainty::KnownAccepted && self.usage.is_some() {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "unaccepted provider attempt contains usage",
                    ));
                }
                if certainty == ProviderSendCertainty::KnownNotSent && self.http_status.is_some() {
                    return Err(OrchError::new(
                        OrchErrorCode::Conflict,
                        "known-not-sent provider attempt contains an HTTP status",
                    ));
                }
            }
            ProviderAttemptState::Admitted => {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "admitted provider attempt contains terminal fields",
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_not_sent_is_same_run_retryable() {
        assert_eq!(
            ProviderSendCertainty::KnownNotSent.retry_class(),
            ProviderRetryClass::SameRunSafe
        );
        assert_eq!(
            ProviderSendCertainty::KnownAccepted.retry_class(),
            ProviderRetryClass::ExplicitNewRunOnly
        );
        assert_eq!(
            ProviderSendCertainty::UncertainAccept.retry_class(),
            ProviderRetryClass::ExplicitNewRunOnly
        );
    }
}
