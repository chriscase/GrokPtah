//! Typed terminal outcomes for a Grok Build run that could not succeed.
//!
//! A run that never reached a provider, was refused by one, or came back with
//! output the host could not parse has *not* completed. Reporting it as
//! completed — or as a bare `Ok` with an empty answer — is the single most
//! expensive lie this system can tell, because every downstream receipt,
//! promotion, and evidence record inherits it.
//!
//! This module is the closed vocabulary that prevents that. Every failure the
//! host can observe maps to exactly one [`RunFailureKind`], each kind maps to
//! exactly one [`RunOutcomeClass`], and [`TerminalVerdict::state`] is
//! structurally incapable of returning [`DurableRunState::Completed`].
//!
//! # What may still be shown
//!
//! Nothing here suppresses the transcript. A blocked or failed run keeps
//! whatever help, partial reasoning, or diagnostic text it produced, and
//! [`TerminalVerdict::retains_transcript_help`] says so explicitly. The rule
//! is only that the *state* must not claim success.

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::launch::LaunchReason;
use crate::run::DurableRunState;

/// How a terminal failure should be understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcomeClass {
    /// The host refused to start or continue. Nothing was spent.
    Blocked,
    /// Work started and ended in a definite failure.
    Failed,
    /// The host cannot tell whether work happened. Never treat as success,
    /// and never treat as a clean no-op either.
    Indeterminate,
}

impl RunOutcomeClass {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Failed => "failed",
            Self::Indeterminate => "indeterminate",
        }
    }

    /// Every class in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 3] = [Self::Blocked, Self::Failed, Self::Indeterminate];

    /// The durable lifecycle state this class records.
    ///
    /// There is deliberately no arm producing [`DurableRunState::Completed`].
    pub const fn state(self) -> DurableRunState {
        match self {
            // A refused launch never ran, so it is not a failure of the work:
            // it is an interruption the operator can act on and retry.
            Self::Blocked => DurableRunState::Interrupted,
            Self::Failed => DurableRunState::Failed,
            // Unknown work may have been performed. `Interrupted` is the only
            // state that demands explicit human recovery rather than implying
            // a clean outcome in either direction.
            Self::Indeterminate => DurableRunState::Interrupted,
        }
    }
}

/// Closed vocabulary of every terminal failure the host can observe.
///
/// There is no `Other(String)` variant: an unclassified failure must be added
/// here deliberately rather than smuggled through as free-form text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFailureKind {
    /// No credential resolved on any route at admission time.
    CredentialMissing,
    /// The provider reported the credential as revoked.
    CredentialRevoked,
    /// The credential's parsed expiry is at or before the attempt.
    CredentialExpired,
    /// A refresh was attempted for an expiring credential and did not succeed.
    CredentialRefreshFailed,
    /// The resolved route no longer matches the route the run was admitted on.
    RouteMismatch,
    /// The resolved model no longer matches the model the run was admitted on.
    ModelMismatch,
    /// The host refused the launch on its own fail-closed evidence.
    LaunchBlocked,
    /// The provider answered `401`.
    ProviderUnauthorized,
    /// The provider answered `429`.
    ProviderRateLimited,
    /// The provider answered with another error status.
    ProviderError,
    /// The request never reached the provider, or the connection broke.
    TransportError,
    /// A response arrived but could not be parsed into a usable turn.
    MalformedOutput,
}

impl RunFailureKind {
    /// The exact wire value used by the JSON contract, and the durable
    /// `error_code` written onto the run record.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CredentialMissing => "credential_missing",
            Self::CredentialRevoked => "credential_revoked",
            Self::CredentialExpired => "credential_expired",
            Self::CredentialRefreshFailed => "credential_refresh_failed",
            Self::RouteMismatch => "route_mismatch",
            Self::ModelMismatch => "model_mismatch",
            Self::LaunchBlocked => "launch_blocked",
            Self::ProviderUnauthorized => "provider_unauthorized",
            Self::ProviderRateLimited => "provider_rate_limited",
            Self::ProviderError => "provider_error",
            Self::TransportError => "transport_error",
            Self::MalformedOutput => "malformed_output",
        }
    }

    /// Every kind in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 12] = [
        Self::CredentialMissing,
        Self::CredentialRevoked,
        Self::CredentialExpired,
        Self::CredentialRefreshFailed,
        Self::RouteMismatch,
        Self::ModelMismatch,
        Self::LaunchBlocked,
        Self::ProviderUnauthorized,
        Self::ProviderRateLimited,
        Self::ProviderError,
        Self::TransportError,
        Self::MalformedOutput,
    ];

    /// How this failure should be understood.
    pub const fn class(self) -> RunOutcomeClass {
        match self {
            // Every credential and configuration failure is caught before the
            // provider is reached, so nothing was spent.
            Self::CredentialMissing
            | Self::CredentialRevoked
            | Self::CredentialExpired
            | Self::CredentialRefreshFailed
            | Self::RouteMismatch
            | Self::ModelMismatch
            | Self::LaunchBlocked
            | Self::ProviderUnauthorized => RunOutcomeClass::Blocked,
            // The provider refused this attempt but the account is fine.
            Self::ProviderRateLimited | Self::ProviderError => RunOutcomeClass::Failed,
            // The request may or may not have been executed, and a reply we
            // cannot parse may or may not describe work that happened.
            Self::TransportError | Self::MalformedOutput => RunOutcomeClass::Indeterminate,
        }
    }

    /// The share-safe cross-product error category for this failure.
    pub const fn error_code(self) -> ErrorCode {
        match self {
            Self::CredentialMissing
            | Self::CredentialRevoked
            | Self::CredentialExpired
            | Self::CredentialRefreshFailed
            | Self::ProviderUnauthorized => ErrorCode::Unauthenticated,
            Self::RouteMismatch | Self::ModelMismatch | Self::LaunchBlocked => {
                ErrorCode::ForbiddenScope
            }
            Self::ProviderRateLimited => ErrorCode::Capacity,
            Self::ProviderError | Self::TransportError => ErrorCode::AuthorityUnavailable,
            Self::MalformedOutput => ErrorCode::Internal,
        }
    }

    /// The full terminal verdict for this failure.
    pub const fn verdict(self) -> TerminalVerdict {
        TerminalVerdict {
            kind: self,
            class: self.class(),
            state: self.class().state(),
            error_code: self.error_code(),
        }
    }

    /// Map a fail-closed launch reason onto the failure it records.
    ///
    /// Ready reasons have no failure to record and return `None`, which is why
    /// this is an `Option` rather than a total function with a fallback.
    pub const fn from_launch_reason(reason: LaunchReason) -> Option<Self> {
        Some(match reason {
            LaunchReason::ResolvedWithFutureExpiry | LaunchReason::ResolvedApiKeyNoExpiryClaim => {
                return None;
            }
            LaunchReason::SignInRequired => Self::CredentialMissing,
            LaunchReason::ReauthenticationRequired => Self::CredentialExpired,
            LaunchReason::RefreshFailed => Self::CredentialRefreshFailed,
            LaunchReason::CredentialRevoked => Self::CredentialRevoked,
            LaunchReason::ModelNotSelected
            | LaunchReason::ModelSelectionUnparseable
            | LaunchReason::ModelRouteMismatch
            | LaunchReason::ModelNotOffered => Self::ModelMismatch,
            LaunchReason::RouteUnrecognized | LaunchReason::RouteProviderMismatch => {
                Self::RouteMismatch
            }
            // Everything else is a host-side fail-closed refusal: the operator
            // needs the launch reason, not a provider-shaped error.
            LaunchReason::CredentialRouteUnrecognized
            | LaunchReason::ExpiryUnparseable
            | LaunchReason::ExpiryNotEstablished
            | LaunchReason::ProviderUnrecognized
            | LaunchReason::BaseEndpointUnset
            | LaunchReason::BaseEndpointInsecure
            | LaunchReason::BaseEndpointMalformed
            | LaunchReason::DialectUnrecognized
            | LaunchReason::CapabilitiesUnprobed
            | LaunchReason::ChatUnsupported
            | LaunchReason::RefreshabilityUnknown => Self::LaunchBlocked,
        })
    }

    /// Classify a provider HTTP status.
    ///
    /// Any non-success status produces a failure: there is no arm that lets a
    /// `4xx` or `5xx` through as a completed turn.
    pub const fn from_http_status(status: u16) -> Option<Self> {
        match status {
            200..=299 => None,
            401 | 403 => Some(Self::ProviderUnauthorized),
            429 => Some(Self::ProviderRateLimited),
            _ => Some(Self::ProviderError),
        }
    }
}

/// The complete typed terminal record for one failed attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TerminalVerdict {
    /// Exactly what failed.
    pub kind: RunFailureKind,
    /// How to understand it.
    pub class: RunOutcomeClass,
    /// The durable lifecycle state to record. Never `completed`.
    pub state: DurableRunState,
    /// The share-safe cross-product error category.
    pub error_code: ErrorCode,
}

impl TerminalVerdict {
    /// The durable `terminal_result` string for this verdict.
    ///
    /// Deliberately distinct from the `error_code`: the result names the
    /// *class* of outcome, and there is no value here that reads as success.
    pub const fn terminal_result(&self) -> &'static str {
        self.class.as_str()
    }

    /// The durable `error_code` string for this verdict.
    pub const fn error_code_str(&self) -> &'static str {
        self.kind.as_str()
    }

    /// Whether a transcript, partial reasoning, or diagnostic help produced
    /// before the failure may still be shown to the operator.
    ///
    /// Always true. A blocked run is still allowed to explain itself; what it
    /// may not do is claim it succeeded.
    pub const fn retains_transcript_help(&self) -> bool {
        true
    }

    /// Whether this verdict could ever be recorded as a successful run.
    ///
    /// Always false, and asserted in tests over every variant so a future
    /// state added to [`DurableRunState`] cannot quietly become a success
    /// path for a failure.
    pub const fn claims_success(&self) -> bool {
        matches!(self.state, DurableRunState::Completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The single most important property in this module: there is no failure,
    /// on any path, that can be recorded as a successful run.
    #[test]
    fn no_failure_kind_can_ever_claim_success() {
        for kind in RunFailureKind::ALL {
            let verdict = kind.verdict();
            assert!(
                !verdict.claims_success(),
                "{kind:?} produced a success-claiming verdict"
            );
            assert_ne!(
                verdict.state,
                DurableRunState::Completed,
                "{kind:?} mapped onto `completed`"
            );
            // `queued` and `running` are not terminal, so a terminal verdict
            // that lands on one would leave a run wedged forever.
            assert!(
                !matches!(
                    verdict.state,
                    DurableRunState::Queued | DurableRunState::Running
                ),
                "{kind:?} mapped onto a non-terminal state"
            );
        }
        for class in RunOutcomeClass::ALL {
            assert_ne!(
                class.state(),
                DurableRunState::Completed,
                "{class:?} mapped onto `completed`"
            );
        }
    }

    #[test]
    fn no_terminal_result_string_reads_as_success() {
        for kind in RunFailureKind::ALL {
            let verdict = kind.verdict();
            let result = verdict.terminal_result();
            for success_word in [
                "ok",
                "completed",
                "complete",
                "success",
                "succeeded",
                "done",
            ] {
                assert_ne!(
                    result, success_word,
                    "{kind:?} produced a success-shaped terminal_result"
                );
            }
            assert!(!result.is_empty(), "{kind:?} produced an empty result");
            assert!(
                !verdict.error_code_str().is_empty(),
                "{kind:?} produced an empty error code"
            );
        }
    }

    /// Every failure named in the requirement maps somewhere, and the mapping
    /// is the one an operator would expect.
    #[test]
    fn each_named_failure_lands_in_the_class_it_belongs_to() {
        for (kind, class) in [
            (RunFailureKind::CredentialMissing, RunOutcomeClass::Blocked),
            (RunFailureKind::CredentialRevoked, RunOutcomeClass::Blocked),
            (RunFailureKind::CredentialExpired, RunOutcomeClass::Blocked),
            (
                RunFailureKind::CredentialRefreshFailed,
                RunOutcomeClass::Blocked,
            ),
            (RunFailureKind::RouteMismatch, RunOutcomeClass::Blocked),
            (RunFailureKind::ModelMismatch, RunOutcomeClass::Blocked),
            (RunFailureKind::LaunchBlocked, RunOutcomeClass::Blocked),
            (
                RunFailureKind::ProviderUnauthorized,
                RunOutcomeClass::Blocked,
            ),
            (RunFailureKind::ProviderRateLimited, RunOutcomeClass::Failed),
            (RunFailureKind::ProviderError, RunOutcomeClass::Failed),
            (
                RunFailureKind::TransportError,
                RunOutcomeClass::Indeterminate,
            ),
            (
                RunFailureKind::MalformedOutput,
                RunOutcomeClass::Indeterminate,
            ),
        ] {
            assert_eq!(kind.class(), class, "{kind:?} landed in the wrong class");
        }
    }

    #[test]
    fn a_provider_status_never_lets_a_non_success_through_as_a_completed_turn() {
        assert_eq!(RunFailureKind::from_http_status(200), None);
        assert_eq!(RunFailureKind::from_http_status(299), None);
        assert_eq!(
            RunFailureKind::from_http_status(401),
            Some(RunFailureKind::ProviderUnauthorized)
        );
        assert_eq!(
            RunFailureKind::from_http_status(403),
            Some(RunFailureKind::ProviderUnauthorized)
        );
        assert_eq!(
            RunFailureKind::from_http_status(429),
            Some(RunFailureKind::ProviderRateLimited)
        );
        // Sweep the whole space: nothing outside 2xx may return `None`.
        for status in 100u16..=599 {
            let classified = RunFailureKind::from_http_status(status);
            if (200..=299).contains(&status) {
                assert_eq!(
                    classified, None,
                    "HTTP {status} was classified as a failure"
                );
            } else {
                let kind = classified
                    .unwrap_or_else(|| panic!("HTTP {status} was let through as a success"));
                assert!(
                    !kind.verdict().claims_success(),
                    "HTTP {status} claimed success"
                );
            }
        }
    }

    #[test]
    fn every_blocking_launch_reason_records_a_failure_and_ready_reasons_do_not() {
        for reason in LaunchReason::ALL {
            let mapped = RunFailureKind::from_launch_reason(reason);
            match reason.readiness() {
                crate::launch::LaunchReadiness::Ready => {
                    assert_eq!(mapped, None, "{reason:?} is ready but recorded a failure")
                }
                _ => {
                    let kind = mapped
                        .unwrap_or_else(|| panic!("{reason:?} refuses but records no failure"));
                    assert!(
                        !kind.verdict().claims_success(),
                        "{reason:?} mapped onto a success-claiming verdict"
                    );
                }
            }
        }
    }

    #[test]
    fn a_refused_or_failed_run_still_keeps_its_transcript_help() {
        // Refusing to claim success must not also erase what the operator can
        // read: the state is the lie we prevent, not the explanation.
        for kind in RunFailureKind::ALL {
            assert!(
                kind.verdict().retains_transcript_help(),
                "{kind:?} suppressed transcript help"
            );
        }
    }

    #[test]
    fn error_categories_stay_share_safe_and_specific() {
        use crate::error::ErrorCode;
        for kind in [
            RunFailureKind::CredentialMissing,
            RunFailureKind::CredentialRevoked,
            RunFailureKind::CredentialExpired,
            RunFailureKind::CredentialRefreshFailed,
            RunFailureKind::ProviderUnauthorized,
        ] {
            assert_eq!(kind.error_code(), ErrorCode::Unauthenticated, "{kind:?}");
        }
        assert_eq!(
            RunFailureKind::ProviderRateLimited.error_code(),
            ErrorCode::Capacity
        );
        for kind in [RunFailureKind::RouteMismatch, RunFailureKind::ModelMismatch] {
            assert_eq!(kind.error_code(), ErrorCode::ForbiddenScope, "{kind:?}");
        }
    }

    #[test]
    fn the_closed_vocabulary_has_unique_stable_wire_values() {
        let mut kinds: Vec<&'static str> = RunFailureKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        let count = kinds.len();
        kinds.sort_unstable();
        kinds.dedup();
        assert_eq!(kinds.len(), count, "duplicate failure wire values");
        for kind in RunFailureKind::ALL {
            assert_eq!(
                serde_json::to_string(&kind).expect("kind serializes"),
                format!("\"{}\"", kind.as_str())
            );
        }
        for class in RunOutcomeClass::ALL {
            assert_eq!(
                serde_json::to_string(&class).expect("class serializes"),
                format!("\"{}\"", class.as_str())
            );
        }
    }

    #[test]
    fn a_verdict_round_trips_through_its_public_encoding() {
        for kind in RunFailureKind::ALL {
            let verdict = kind.verdict();
            let encoded = serde_json::to_value(verdict).expect("verdict serializes");
            let decoded: TerminalVerdict =
                serde_json::from_value(encoded.clone()).expect("verdict round-trips");
            assert_eq!(decoded, verdict, "{kind:?} did not round-trip");
            // deny_unknown_fields: a verdict with an extra claim is not one.
            let mut doctored = encoded;
            doctored["completed"] = serde_json::Value::Bool(true);
            assert!(
                serde_json::from_value::<TerminalVerdict>(doctored).is_err(),
                "{kind:?} accepted an unknown field"
            );
        }
    }
}
