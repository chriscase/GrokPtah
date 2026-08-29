//! What a transport failure proves about delivery.
//!
//! This is a classification rule, not a ledger: no state, no records, no
//! permits, no ordinals, and nothing to persist. The durable attempt lattice
//! that *does* hold those things is #497's G3, and this module deliberately
//! does not approximate it.
//!
//! The rule exists because `main` currently retries a provider request on any
//! transport error, including a timeout. A timeout can happen after the request
//! bytes were fully written and the provider has already done the work, so
//! re-sending it duplicates a model invocation rather than recovering a lost
//! one. Only a connection that was never established proves otherwise.
//!
//! This is #478's acceptance criterion stated as a function: *automatic retries
//! stand down for any attempt whose delivery is not proven to have not
//! happened.*

/// What the host honestly knows about whether a request reached the provider.
///
/// Three answers, because two is not enough: a request that provably never
/// left, one that provably arrived, and — the common case after a write begins
/// — one the host cannot decide. Collapsing the third into either of the others
/// is how a retry loop duplicates work or a projection claims certainty it does
/// not have.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryKnowledge {
    /// The connection was never established, so no request byte can have moved.
    KnownNotDelivered,
    /// The provider answered — with a completion, a rejection, or bytes that
    /// could not be parsed. The exchange is settled: whatever happened, it
    /// happened, and a fresh request is a *new* request rather than a retry.
    KnownDelivered,
    /// The request may or may not have been delivered. Not an error state, and
    /// not a licence to retry — it is the honest answer when the host cannot
    /// tell, which is most transport failures after a write begins.
    Unknown,
}

impl DeliveryKnowledge {
    /// The single retry rule: an automatic retry is admissible only when
    /// delivery is *proven* not to have happened.
    ///
    /// Anything else — a timeout, a reset after the write, a decode failure —
    /// keeps its uncertainty and must not be re-sent. Recovering from those is
    /// an explicit reconciliation decision, not something a loop may take on
    /// its own.
    pub fn may_auto_retry(self) -> bool {
        matches!(self, Self::KnownNotDelivered)
    }

    /// Whether the exchange is settled: the provider answered, so this attempt
    /// has an outcome even if the host could not read it.
    pub fn is_settled(self) -> bool {
        matches!(self, Self::KnownDelivered)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotDelivered => "not_sent",
            Self::KnownDelivered => "settled",
            Self::Unknown => "uncertain",
        }
    }
}

/// Classify a transport failure from the two facts a client can report.
///
/// `is_timeout` dominates deliberately. A connect *timeout* is not a refused
/// connection: the peer may have accepted and read the request while the
/// client gave up waiting, so it cannot prove non-delivery even though the
/// client also reports it as a connect-phase failure.
pub fn classify_transport_failure(is_connect: bool, is_timeout: bool) -> DeliveryKnowledge {
    if is_connect && !is_timeout {
        DeliveryKnowledge::KnownNotDelivered
    } else {
        DeliveryKnowledge::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refused_connection_proves_a_request_was_not_sent() {
        assert_eq!(
            classify_transport_failure(true, false),
            DeliveryKnowledge::KnownNotDelivered
        );
        for (is_connect, is_timeout) in [(false, true), (true, true), (false, false)] {
            assert_eq!(
                classify_transport_failure(is_connect, is_timeout),
                DeliveryKnowledge::Unknown,
                "connect={is_connect} timeout={is_timeout} cannot prove non-delivery"
            );
        }
    }

    #[test]
    fn a_connect_timeout_does_not_count_as_a_refused_connection() {
        // The peer may have accepted and read the request while the client
        // stopped waiting, so this is uncertain even though it is connect-phase.
        assert!(!classify_transport_failure(true, true).may_auto_retry());
    }

    #[test]
    fn only_known_not_delivered_may_auto_retry() {
        assert!(DeliveryKnowledge::KnownNotDelivered.may_auto_retry());
        assert!(!DeliveryKnowledge::Unknown.may_auto_retry());
        assert!(
            !DeliveryKnowledge::KnownDelivered.may_auto_retry(),
            "a settled exchange is not re-sent; a fresh request is a new request"
        );
    }

    #[test]
    fn the_three_answers_stay_distinct() {
        assert!(DeliveryKnowledge::KnownDelivered.is_settled());
        assert!(!DeliveryKnowledge::Unknown.is_settled());
        assert!(!DeliveryKnowledge::KnownNotDelivered.is_settled());
        assert_eq!(DeliveryKnowledge::KnownNotDelivered.as_str(), "not_sent");
        assert_eq!(DeliveryKnowledge::Unknown.as_str(), "uncertain");
        assert_eq!(DeliveryKnowledge::KnownDelivered.as_str(), "settled");
    }
}
