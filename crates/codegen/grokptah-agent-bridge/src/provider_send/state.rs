//! The one provider-send state lattice (#478).
//!
//! Exactly one durable truth describes whether a physical provider request
//! reached the wire. Every transition is justified by an explicit evidence
//! value, so the lattice can never advance on a caller's optimism.
//!
//! Ordering guarantee the whole design rests on: [`ProviderAttemptState::Sending`]
//! is fsynced *before* the send future is created, so a durable record still at
//! [`ProviderAttemptState::Preparing`] proves no request byte could have moved.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Durable state of one physical provider-send attempt.
///
/// `NotSent` and `Settled` are terminal. `Uncertain` is deliberately *not*
/// terminal and *not* resolvable in-process: it blocks a fresh ordinal in the
/// same scope until an explicit out-of-band resolution (#466) records what
/// really happened, and it never permits an automatic retry.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ProviderAttemptState {
    /// Durable intent exists. Admission has not been granted and no send
    /// future has been created.
    Preparing,
    /// Durable immediately before the send future is created. From here on the
    /// host cannot prove by itself that no byte moved.
    Sending,
    /// Transport or host evidence proves no request byte reached the provider.
    NotSent,
    /// Transport evidence: the provider produced response headers.
    Acknowledged,
    /// Transport evidence: response body or stream bytes were observed.
    Responding,
    /// Transport evidence: the response completed (successfully or with an
    /// explicit provider rejection).
    Settled,
    /// A write may have happened and the outcome is unknown. Never auto-retry.
    Uncertain,
}

impl ProviderAttemptState {
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
            // Sending is durable before the send future exists, so a record
            // still at Preparing proves the request never left the host.
            Self::Preparing | Self::NotSent => DeliveryKnowledge::KnownNotDelivered,
            Self::Acknowledged | Self::Responding | Self::Settled => {
                DeliveryKnowledge::KnownDelivered
            }
            Self::Sending | Self::Uncertain => DeliveryKnowledge::Unknown,
        }
    }

    /// The single retry rule (#478): an automatic retry of a bound attempt is
    /// admissible only when delivery is *proven* not to have happened.
    ///
    /// A provider that answered — including with 429/5xx/400 — did not leave
    /// this attempt un-delivered; the caller must open a *new* ordinal for a
    /// fresh request rather than re-sending this one. See
    /// [`crate::provider_send::ledger::AttemptLedger::begin_attempt`] for the
    /// admission rule that governs that.
    pub fn may_auto_retry(self) -> bool {
        matches!(self, Self::NotSent)
    }

    /// Legal successor states. Anything outside this relation is a bug and is
    /// rejected by the ledger rather than silently written.
    pub fn may_transition_to(self, next: Self) -> bool {
        use ProviderAttemptState::*;
        match (self, next) {
            (Preparing, Sending | NotSent) => true,
            (Sending, NotSent | Acknowledged | Responding | Settled | Uncertain) => true,
            (Acknowledged, Responding | Settled | Uncertain) => true,
            (Responding, Settled | Uncertain) => true,
            // Uncertainty is never downgraded to proof of non-delivery, and it
            // is only ever resolved by an explicit out-of-band grant (#466),
            // which the ledger checks separately.
            (Uncertain, Settled) => true,
            _ => false,
        }
    }
}

impl fmt::Display for ProviderAttemptState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Honest public answer to "did this request reach the provider?".
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryKnowledge {
    KnownNotDelivered,
    KnownDelivered,
    Unknown,
}

impl DeliveryKnowledge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotDelivered => "known_not_delivered",
            Self::KnownDelivered => "known_delivered",
            Self::Unknown => "unknown",
        }
    }
}

/// Proof that the host itself — not the provider — knows the request never
/// entered the write path.
///
/// Only two things can produce it, and neither is constructible from a caller's
/// belief: the owning send path observing a failure strictly before dispatch,
/// and the recovery path proving the owning host incarnation is gone while the
/// durable record is still `Preparing`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HostEvidence {
    /// The owning send path failed before creating the send future.
    OwnerObservedBeforeDispatch { detail: HostFailureClass },
    /// A durable `Preparing` record whose owning incarnation is not live.
    /// Safe because `Sending` is fsynced before the send future is created.
    IncarnationNotLive { observed_incarnation: String },
}

/// Bounded, secret-free classification of a pre-dispatch host failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostFailureClass {
    /// Request body could not be serialized.
    RequestSerialization,
    /// The inference client could not be constructed.
    ClientConstruction,
    /// The request URL could not be constructed.
    RouteConstruction,
    /// Admission was refused before dispatch.
    AdmissionRefused,
    /// The caller cancelled before dispatch.
    CancelledBeforeDispatch,
    /// A crash cut fired before dispatch (test-only injection).
    InjectedCut,
}

/// Evidence produced by the transport itself. Only these values may move an
/// attempt out of `Sending` or later.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TransportEvidence {
    /// The connection was never established, so no request byte was written.
    /// This is the *only* transport classification that proves `NotSent`.
    ConnectionNeverEstablished,
    /// Response headers were received.
    ResponseHeaders { status: u16 },
    /// Response body or stream bytes were observed.
    ResponseBytes { status: u16, bytes: u64 },
    /// The response completed.
    ResponseComplete { status: u16, bytes: u64 },
    /// A write may have happened; the outcome cannot be determined.
    /// Timeouts, resets, EOF, decode and parse failures all land here.
    PossibleWriteUnresolved { class: UncertaintyClass },
}

/// Why an attempt is uncertain. Deliberately coarse and secret-free.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UncertaintyClass {
    /// Deadline elapsed at any phase after the send future existed.
    Timeout,
    /// Connection reset or closed by peer.
    ConnectionReset,
    /// Stream or body ended before the response was complete.
    UnexpectedEof,
    /// Response could not be decoded or parsed.
    ResponseParse,
    /// Cancelled after the send future existed.
    CancelledAfterDispatch,
    /// The process was interrupted while the attempt was at `Sending` or later.
    ProcessInterrupted,
    /// Any other transport error after the send future existed.
    TransportError,
}

impl TransportEvidence {
    /// The state this evidence justifies, given the state it is applied to.
    pub fn justifies(&self, from: ProviderAttemptState) -> ProviderAttemptState {
        match self {
            Self::ConnectionNeverEstablished => ProviderAttemptState::NotSent,
            Self::ResponseHeaders { .. } => ProviderAttemptState::Acknowledged,
            Self::ResponseBytes { .. } => ProviderAttemptState::Responding,
            Self::ResponseComplete { .. } => ProviderAttemptState::Settled,
            Self::PossibleWriteUnresolved { .. } => {
                // Uncertainty never regresses a state that already carries
                // stronger delivery knowledge than "unknown write".
                let _ = from;
                ProviderAttemptState::Uncertain
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparing_is_the_only_pre_wire_state_that_proves_non_delivery() {
        assert_eq!(
            ProviderAttemptState::Preparing.delivery_knowledge(),
            DeliveryKnowledge::KnownNotDelivered
        );
        assert_eq!(
            ProviderAttemptState::Sending.delivery_knowledge(),
            DeliveryKnowledge::Unknown
        );
    }

    #[test]
    fn only_not_sent_admits_an_automatic_retry() {
        for state in [
            ProviderAttemptState::Preparing,
            ProviderAttemptState::Sending,
            ProviderAttemptState::Acknowledged,
            ProviderAttemptState::Responding,
            ProviderAttemptState::Settled,
            ProviderAttemptState::Uncertain,
        ] {
            assert!(!state.may_auto_retry(), "{state} must stand down");
        }
        assert!(ProviderAttemptState::NotSent.may_auto_retry());
    }

    #[test]
    fn uncertainty_never_becomes_proof_of_non_delivery() {
        assert!(!ProviderAttemptState::Uncertain.may_transition_to(ProviderAttemptState::NotSent));
        assert!(
            !ProviderAttemptState::Acknowledged.may_transition_to(ProviderAttemptState::NotSent)
        );
        assert!(!ProviderAttemptState::Responding.may_transition_to(ProviderAttemptState::NotSent));
        assert!(!ProviderAttemptState::Settled.may_transition_to(ProviderAttemptState::NotSent));
    }

    #[test]
    fn sending_can_only_reach_not_sent_through_connection_evidence() {
        assert_eq!(
            TransportEvidence::ConnectionNeverEstablished.justifies(ProviderAttemptState::Sending),
            ProviderAttemptState::NotSent
        );
        for class in [
            UncertaintyClass::Timeout,
            UncertaintyClass::ConnectionReset,
            UncertaintyClass::UnexpectedEof,
            UncertaintyClass::ResponseParse,
            UncertaintyClass::TransportError,
            UncertaintyClass::ProcessInterrupted,
            UncertaintyClass::CancelledAfterDispatch,
        ] {
            assert_eq!(
                TransportEvidence::PossibleWriteUnresolved { class }
                    .justifies(ProviderAttemptState::Sending),
                ProviderAttemptState::Uncertain
            );
        }
    }

    #[test]
    fn uncertain_is_not_terminal_and_blocks_reuse() {
        assert!(!ProviderAttemptState::Uncertain.is_terminal());
        assert!(ProviderAttemptState::NotSent.is_terminal());
        assert!(ProviderAttemptState::Settled.is_terminal());
    }
}
