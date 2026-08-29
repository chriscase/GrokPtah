//! The one provider-send lattice.
//!
//! Exactly one value describes whether a physical provider request reached the
//! wire, and every transition is justified by explicit evidence rather than by
//! a caller's optimism.
//!
//! The ordering the whole design rests on: [`SendState::Sending`] becomes
//! durable *before* the send future exists, so a record still at
//! [`SendState::Preparing`] proves no request byte moved, and a record at
//! `Sending` or later proves nothing either way.
//!
//! This module is deliberately provider-neutral and transport-free. It names
//! no HTTP client, no URL and no dialect, so the lattice can be tested
//! exhaustively without contacting anything. Binding it to a real transport is
//! the caller's job, and [`SendPermit`] makes an unbound call site fail to
//! compile rather than silently escaping the ledger.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use super::retry::{RetryBudget, RetryDecision, StandDownReason};

/// Maximum attempt records retained per scope before the oldest terminal ones
/// are dropped. Bounds ledger growth; non-terminal records are never dropped.
pub const MAX_RETAINED_ATTEMPTS: usize = 64;

/// Durable state of one physical provider-send attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    /// Durable intent exists; no send future has been created.
    Preparing,
    /// Durable immediately before the send future is created.
    Sending,
    /// Evidence proves no request byte reached the provider.
    NotSent,
    /// Evidence: the provider produced response headers.
    Acknowledged,
    /// Evidence: response body or stream bytes were observed.
    Responding,
    /// Evidence: the exchange completed, successfully or with an explicit
    /// provider rejection.
    Settled,
    /// A write may have happened and the outcome is unknown. Never auto-retry.
    Uncertain,
}

impl SendState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Sending => "sending",
            Self::NotSent => "not_sent",
            Self::Acknowledged => "acknowledged",
            Self::Responding => "responding",
            Self::Settled => "settled",
            Self::Uncertain => "uncertain",
        }
    }

    /// No further transition is possible and the scope may admit a new ordinal.
    ///
    /// `Uncertain` is deliberately excluded: it is not terminal and is not
    /// resolvable in-process.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::NotSent | Self::Settled)
    }

    /// The attempt is still owned by a live send path.
    pub fn is_in_flight(self) -> bool {
        matches!(
            self,
            Self::Preparing | Self::Sending | Self::Acknowledged | Self::Responding
        )
    }

    /// What the host honestly knows about delivery of the request bytes.
    pub fn delivery_knowledge(self) -> DeliveryKnowledge {
        match self {
            Self::Preparing | Self::NotSent => DeliveryKnowledge::KnownNotDelivered,
            Self::Acknowledged | Self::Responding | Self::Settled => {
                DeliveryKnowledge::KnownDelivered
            }
            Self::Sending | Self::Uncertain => DeliveryKnowledge::Unknown,
        }
    }

    /// The single retry rule: an automatic retry is admissible only when
    /// delivery is *proven* not to have happened.
    pub fn may_auto_retry(self) -> bool {
        matches!(self, Self::NotSent)
    }

    /// Legal successors. Anything outside this relation is refused, not written.
    pub fn may_transition_to(self, next: Self) -> bool {
        use SendState::*;
        match (self, next) {
            (Preparing, Sending | NotSent) => true,
            (Sending, NotSent | Acknowledged | Responding | Settled | Uncertain) => true,
            (Acknowledged, Responding | Settled | Uncertain) => true,
            (Responding, Settled | Uncertain) => true,
            // Uncertainty is never downgraded to proof of non-delivery. It is
            // resolved only by an explicit out-of-band grant.
            (Uncertain, Settled) => true,
            _ => false,
        }
    }
}

impl fmt::Display for SendState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Honest public answer to "did this request reach the provider?".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKnowledge {
    KnownNotDelivered,
    KnownDelivered,
    Unknown,
}

/// Abstract transport evidence.
///
/// Naming the evidence rather than the error type keeps the classification
/// rules testable without a network stack, and keeps this module free of any
/// particular HTTP client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportEvidence {
    /// The connection was refused before any byte could be written.
    ConnectionRefused,
    /// A timeout. The request may or may not have been written.
    TimedOut,
    /// The peer reset the connection after the write began.
    ResetAfterWrite,
    /// Response headers were received.
    ResponseHeaders,
    /// Response body or stream bytes were observed.
    ResponseBytes,
    /// The response completed and was parsed.
    ResponseComplete,
    /// The provider answered with an explicit rejection (any status).
    ProviderRejected,
    /// The response could not be decoded or parsed.
    DecodeFailed,
    /// The reader was dropped part-way through the response.
    ReaderAbandoned,
    /// The host cancelled before the send future was created.
    CancelledBeforeDispatch,
}

impl TransportEvidence {
    /// The state this evidence justifies, given the state it arrives in.
    ///
    /// Only a refused connection yields `NotSent`. Timeout, reset, decode and
    /// parse failures all preserve uncertainty, because each of them can occur
    /// after bytes were written.
    pub fn classify(self, current: SendState) -> SendState {
        match self {
            Self::ConnectionRefused => SendState::NotSent,
            Self::CancelledBeforeDispatch => {
                if current == SendState::Preparing {
                    SendState::NotSent
                } else {
                    SendState::Uncertain
                }
            }
            Self::ResponseHeaders => SendState::Acknowledged,
            Self::ResponseBytes => SendState::Responding,
            // A provider that answered — including with a rejection — settles
            // this attempt. A fresh request needs a new ordinal.
            Self::ResponseComplete | Self::ProviderRejected => SendState::Settled,
            // A clean end of stream is not a settlement: a response the host
            // cannot parse says nothing about what the provider did.
            Self::TimedOut | Self::ResetAfterWrite | Self::DecodeFailed | Self::ReaderAbandoned => {
                SendState::Uncertain
            }
        }
    }
}

/// Refusals the ledger can return. Every one leaves the ledger unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendError {
    /// A prior attempt in this scope is not terminal.
    ScopeBlocked { ordinal: u64, state: SendState },
    /// The transition is not in the lattice.
    IllegalTransition { from: SendState, to: SendState },
    /// No attempt with that ordinal exists in this scope.
    UnknownAttempt { ordinal: u64 },
    /// The bundle contradicts itself and was refused before being written.
    ContradictoryBundle { detail: &'static str },
    /// Resolving an uncertain attempt needs an explicit grant.
    ResolutionNotGranted,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScopeBlocked { ordinal, state } => {
                write!(
                    f,
                    "attempt {ordinal} is {state}; scope cannot admit a new send"
                )
            }
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal send transition {from} -> {to}")
            }
            Self::UnknownAttempt { ordinal } => write!(f, "unknown attempt {ordinal}"),
            Self::ContradictoryBundle { detail } => {
                write!(f, "contradictory settlement bundle: {detail}")
            }
            Self::ResolutionNotGranted => {
                f.write_str("resolving an uncertain attempt requires an explicit operator grant")
            }
        }
    }
}

impl std::error::Error for SendError {}

/// Proof that a send was admitted by the ledger.
///
/// Only [`SendLedger::begin`] can mint one, and a dispatch function that takes
/// a `SendPermit` cannot be called from an unbound site. This is the structural
/// gate that keeps a second send path from appearing.
#[derive(Debug)]
pub struct SendPermit {
    ordinal: u64,
    request_digest: super::observation::RawObservationDigest,
}

impl SendPermit {
    pub fn ordinal(&self) -> u64 {
        self.ordinal
    }

    pub fn request_digest(&self) -> super::observation::RawObservationDigest {
        self.request_digest
    }
}

/// One attempt's durable shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAttempt {
    pub ordinal: u64,
    pub state: SendState,
    pub request_digest: super::observation::RawObservationDigest,
    /// Set exactly once, atomically with the settling transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
    #[serde(default)]
    pub audit_accounted: bool,
}

impl SendAttempt {
    /// A bundle that cannot be true is refused before it is written.
    fn validate(&self) -> Result<(), SendError> {
        if self.receipt.is_some()
            && self.state.delivery_knowledge() != DeliveryKnowledge::KnownDelivered
        {
            return Err(SendError::ContradictoryBundle {
                detail: "a receipt implies delivery",
            });
        }
        if self.audit_accounted && !self.state.is_terminal() {
            return Err(SendError::ContradictoryBundle {
                detail: "a non-terminal attempt cannot be audit-accounted",
            });
        }
        Ok(())
    }
}

/// The single send authority for one scope.
///
/// In-memory and bounded. Persisting it is the job of the layer above G1–G4
/// that owns durable identity; this type deliberately does not invent one.
#[derive(Debug, Default)]
pub struct SendLedger {
    attempts: BTreeMap<u64, SendAttempt>,
    next_ordinal: u64,
}

impl SendLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild after a restart from whatever records survived.
    ///
    /// The next ordinal is derived from the maximum seen, so a crash between
    /// allocation and use never reissues an ordinal.
    pub fn recover(attempts: impl IntoIterator<Item = SendAttempt>) -> Self {
        let attempts: BTreeMap<u64, SendAttempt> =
            attempts.into_iter().map(|a| (a.ordinal, a)).collect();
        let next_ordinal = attempts
            .keys()
            .copied()
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        Self {
            attempts,
            next_ordinal,
        }
    }

    /// The attempt, if any, that blocks a new send in this scope.
    pub fn blocking_attempt(&self) -> Option<&SendAttempt> {
        self.attempts.values().find(|a| !a.state.is_terminal())
    }

    /// Admit a new physical send.
    ///
    /// Writes `Preparing` first, which is what makes an interrupted attempt
    /// provably un-sent. Refuses while any earlier attempt is non-terminal, so
    /// an `Uncertain` attempt can never be silently reopened under a fresh
    /// ordinal.
    pub fn begin(
        &mut self,
        request_digest: super::observation::RawObservationDigest,
    ) -> Result<SendPermit, SendError> {
        if let Some(blocker) = self.blocking_attempt() {
            return Err(SendError::ScopeBlocked {
                ordinal: blocker.ordinal,
                state: blocker.state,
            });
        }
        let ordinal = self.next_ordinal.max(1);
        self.next_ordinal = ordinal.saturating_add(1);
        self.attempts.insert(
            ordinal,
            SendAttempt {
                ordinal,
                state: SendState::Preparing,
                request_digest,
                receipt: None,
                audit_accounted: false,
            },
        );
        self.prune();
        Ok(SendPermit {
            ordinal,
            request_digest,
        })
    }

    /// Mark the attempt `Sending`. Callers must do this *before* creating the
    /// send future; after it returns, the host can no longer prove non-delivery.
    pub fn mark_sending(&mut self, permit: &SendPermit) -> Result<(), SendError> {
        self.transition(permit.ordinal, SendState::Sending)
    }

    /// Apply transport evidence.
    pub fn observe(
        &mut self,
        permit: &SendPermit,
        evidence: TransportEvidence,
    ) -> Result<SendState, SendError> {
        let current = self
            .attempts
            .get(&permit.ordinal)
            .ok_or(SendError::UnknownAttempt {
                ordinal: permit.ordinal,
            })?
            .state;
        let next = evidence.classify(current);
        if next == current {
            return Ok(current);
        }
        self.transition(permit.ordinal, next)?;
        Ok(next)
    }

    /// Settle an attempt together with its receipt and audit accounting.
    ///
    /// One write, so settlement, receipt and accounting cannot split into
    /// contradictory durable states.
    pub fn settle(
        &mut self,
        permit: &SendPermit,
        receipt: Option<String>,
        audit_accounted: bool,
    ) -> Result<(), SendError> {
        let attempt = self
            .attempts
            .get(&permit.ordinal)
            .ok_or(SendError::UnknownAttempt {
                ordinal: permit.ordinal,
            })?;
        if !attempt.state.may_transition_to(SendState::Settled) {
            return Err(SendError::IllegalTransition {
                from: attempt.state,
                to: SendState::Settled,
            });
        }
        let candidate = SendAttempt {
            ordinal: attempt.ordinal,
            state: SendState::Settled,
            request_digest: attempt.request_digest,
            receipt,
            audit_accounted,
        };
        candidate.validate()?;
        self.attempts.insert(candidate.ordinal, candidate);
        Ok(())
    }

    /// Resolve an `Uncertain` attempt. Requires an explicit grant, and never
    /// performs provider I/O of its own.
    pub fn resolve_uncertain(
        &mut self,
        ordinal: u64,
        granted: bool,
        outcome: SendState,
    ) -> Result<(), SendError> {
        if !granted {
            return Err(SendError::ResolutionNotGranted);
        }
        self.transition(ordinal, outcome)
    }

    /// The retry answer for a bound attempt.
    pub fn retry_decision(&self, ordinal: u64, budget: &mut RetryBudget) -> RetryDecision {
        let Some(attempt) = self.attempts.get(&ordinal) else {
            return RetryDecision::StandDown {
                reason: StandDownReason::NotTransient,
            };
        };
        let permitted = match attempt.state.delivery_knowledge() {
            DeliveryKnowledge::KnownNotDelivered if attempt.state.may_auto_retry() => Ok(()),
            DeliveryKnowledge::KnownNotDelivered => Ok(()),
            DeliveryKnowledge::KnownDelivered => Err(StandDownReason::AlreadyDelivered),
            DeliveryKnowledge::Unknown => Err(StandDownReason::DeliveryUnproven),
        };
        budget.next(permitted)
    }

    pub fn attempt(&self, ordinal: u64) -> Option<&SendAttempt> {
        self.attempts.get(&ordinal)
    }

    pub fn len(&self) -> usize {
        self.attempts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.attempts.is_empty()
    }

    fn transition(&mut self, ordinal: u64, next: SendState) -> Result<(), SendError> {
        let attempt = self
            .attempts
            .get_mut(&ordinal)
            .ok_or(SendError::UnknownAttempt { ordinal })?;
        if !attempt.state.may_transition_to(next) {
            return Err(SendError::IllegalTransition {
                from: attempt.state,
                to: next,
            });
        }
        attempt.state = next;
        Ok(())
    }

    /// Bound retention. Only terminal attempts are ever dropped, so an
    /// unresolved `Uncertain` record can never be aged out of existence.
    fn prune(&mut self) {
        while self.attempts.len() > MAX_RETAINED_ATTEMPTS {
            let Some(victim) = self
                .attempts
                .iter()
                .find(|(_, a)| a.state.is_terminal())
                .map(|(k, _)| *k)
            else {
                break;
            };
            self.attempts.remove(&victim);
        }
    }
}
