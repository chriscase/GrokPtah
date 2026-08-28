//! Offline verifier for independently replayable adaptive evidence.
//!
//! Replay records contain IDs, digests, typed outcomes, profile decisions,
//! recovery, latency, and provider-reported usage only. They never contain
//! screenshots, credentials, paths, prompts, raw policy, or action payloads.

use serde::{Deserialize, Serialize};

use super::policy::{ProfileReason, ProfileTransition};
use super::profile::AdaptiveProfile;

const MAX_EVENTS: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayEventKind {
    Observation,
    Decision,
    ActionProposal,
    ActionResult,
    Recovery,
    Transition,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReplayEvent {
    pub sequence: u64,
    pub kind: ReplayEventKind,
    pub observation_id: Option<String>,
    pub observation_digest: Option<String>,
    pub profile: AdaptiveProfile,
    pub reason: Option<ProfileReason>,
    pub action_digest: Option<String>,
    pub result_code: Option<String>,
    pub recovery_code: Option<String>,
    pub latency_millis: Option<u64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplaySummary {
    pub event_count: usize,
    pub observation_count: usize,
    pub transition_count: usize,
    pub terminal: bool,
    pub provider_usage_known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplayError {
    TooManyEvents,
    NonMonotonicSequence,
    MissingObservationEvidence,
    InvalidDigest,
    InvalidActionDigest,
    InvalidTransition,
    InvalidLatency,
    TerminalNotLast,
    MissingTerminal,
}

impl std::fmt::Display for ReplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::TooManyEvents => "replay exceeds the evidence event bound",
            Self::NonMonotonicSequence => "replay sequence is not strictly monotonic",
            Self::MissingObservationEvidence => "observation is missing its id or digest",
            Self::InvalidDigest => "replay contains an invalid digest",
            Self::InvalidActionDigest => "action evidence is missing its digest",
            Self::InvalidTransition => "profile transition is not one bounded escalation",
            Self::InvalidLatency => "provider latency is outside the bounded range",
            Self::TerminalNotLast => "terminal evidence has trailing events",
            Self::MissingTerminal => "replay has no terminal recovery/result event",
        })
    }
}

impl std::error::Error for ReplayError {}

pub struct ReplayVerifier;

impl ReplayVerifier {
    pub fn verify(events: &[ReplayEvent]) -> Result<ReplaySummary, ReplayError> {
        if events.len() > MAX_EVENTS {
            return Err(ReplayError::TooManyEvents);
        }
        let mut previous_sequence = None;
        let mut previous_profile = None;
        let mut observations = 0;
        let mut transitions = 0;
        let mut provider_usage_known = false;
        let mut terminal = false;

        for (index, event) in events.iter().enumerate() {
            if previous_sequence.is_some_and(|previous| event.sequence <= previous) {
                return Err(ReplayError::NonMonotonicSequence);
            }
            previous_sequence = Some(event.sequence);
            if terminal {
                return Err(ReplayError::TerminalNotLast);
            }
            if event
                .latency_millis
                .is_some_and(|latency| latency > 60 * 60 * 1_000)
            {
                return Err(ReplayError::InvalidLatency);
            }
            if event.prompt_tokens.is_some() || event.completion_tokens.is_some() {
                provider_usage_known = true;
            }
            match event.kind {
                ReplayEventKind::Observation => {
                    observations += 1;
                    if event.observation_id.as_deref().is_none_or(str::is_empty)
                        || !valid_digest(event.observation_digest.as_deref())
                    {
                        return Err(
                            if event.observation_id.as_deref().is_none_or(str::is_empty) {
                                ReplayError::MissingObservationEvidence
                            } else {
                                ReplayError::InvalidDigest
                            },
                        );
                    }
                }
                ReplayEventKind::ActionProposal => {
                    if !valid_digest(event.action_digest.as_deref()) {
                        return Err(ReplayError::InvalidActionDigest);
                    }
                }
                ReplayEventKind::Transition => {
                    let Some(from) = previous_profile else {
                        return Err(ReplayError::InvalidTransition);
                    };
                    if event.profile != from || event.reason.is_none() {
                        return Err(ReplayError::InvalidTransition);
                    }
                    // The next profile is represented by the following
                    // decision event; it must be one rung above this one.
                    transitions += 1;
                }
                ReplayEventKind::Terminal | ReplayEventKind::Recovery => {
                    terminal = true;
                    if index + 1 != events.len() {
                        return Err(ReplayError::TerminalNotLast);
                    }
                }
                ReplayEventKind::Decision | ReplayEventKind::ActionResult => {}
            }
            previous_profile = Some(event.profile);
        }
        if !terminal {
            return Err(ReplayError::MissingTerminal);
        }

        Ok(ReplaySummary {
            event_count: events.len(),
            observation_count: observations,
            transition_count: transitions,
            terminal,
            provider_usage_known,
        })
    }

    pub fn verify_json(raw: &str) -> Result<ReplaySummary, ReplayError> {
        let events: Vec<ReplayEvent> =
            serde_json::from_str(raw).map_err(|_| ReplayError::MissingObservationEvidence)?;
        Self::verify(&events)
    }

    /// Validate a single controller transition without trusting caller text.
    pub fn verify_transition(
        from: AdaptiveProfile,
        transition: &ProfileTransition,
    ) -> Result<(), ReplayError> {
        match transition {
            ProfileTransition::Escalate {
                from: actual_from,
                to,
                ..
            } if actual_from == &from && from.escalated() == Some(*to) => Ok(()),
            ProfileTransition::Stop(_) => Ok(()),
            ProfileTransition::Escalate { .. } => Err(ReplayError::InvalidTransition),
        }
    }
}

fn valid_digest(value: Option<&str>) -> bool {
    value.is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(sequence: u64, kind: ReplayEventKind) -> ReplayEvent {
        ReplayEvent {
            sequence,
            kind,
            observation_id: Some("observation-1".into()),
            observation_digest: Some("a".repeat(64)),
            profile: AdaptiveProfile::Economy,
            reason: Some(ProfileReason::RoutineTask),
            action_digest: Some("b".repeat(64)),
            result_code: None,
            recovery_code: None,
            latency_millis: None,
            prompt_tokens: None,
            completion_tokens: None,
        }
    }

    #[test]
    fn replay_requires_redacted_evidence_and_terminal_cut() {
        let events = vec![
            event(1, ReplayEventKind::Observation),
            event(2, ReplayEventKind::Decision),
            event(3, ReplayEventKind::Terminal),
        ];
        let summary = ReplayVerifier::verify(&events).unwrap();
        assert!(summary.terminal);
        assert_eq!(summary.observation_count, 1);
    }

    #[test]
    fn malformed_or_unfinished_replays_fail_closed() {
        let mut missing_digest = event(1, ReplayEventKind::Observation);
        missing_digest.observation_digest = None;
        assert_eq!(
            ReplayVerifier::verify(&[missing_digest]).unwrap_err(),
            ReplayError::MissingObservationEvidence
        );
        assert_eq!(
            ReplayVerifier::verify(&[event(1, ReplayEventKind::Decision)]).unwrap_err(),
            ReplayError::MissingTerminal
        );
    }
}
