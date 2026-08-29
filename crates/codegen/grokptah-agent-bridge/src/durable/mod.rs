//! Durable agent core: the semantics a long-running agent needs to be safe.
//!
//! This is canonical train #2 of the #492 consolidation plan — *durable agent,
//! SDK, external-worker and embeddable manager*. It is **not** the G1–G4 host
//! authority/effect train and deliberately reimplements none of it: host
//! lifecycle and shutdown sealing (#455/#468), canonical principal and auth
//! generations (#477/#460), capability/effect generations and queue ownership
//! (#458/#461), and audit-v2 (#462/#469) all remain G1–G4's.
//!
//! Where this core has to refer to one of those authorities it does so through
//! a typed marker that records the authority as *provisional*
//! ([`sdk::GrantProvenance`]), so a provisional identity can never be read back
//! as a canonical one once G1–G4 lands.
//!
//! # What the modules hold
//!
//! - [`observation`] — typed terminal observations, and the rule that a digest
//!   is taken from raw output *before* any bounded projection of it.
//! - [`progress`] — stationarity that distinguishes a stuck turn from a
//!   productive wait, so a run that is advancing is never stopped as a no-op.
//! - [`send`] — the one provider-send lattice, preserving the
//!   not-sent / uncertain / settled distinction.
//! - [`retry`] — bounded budgets and typed retry decisions derived from
//!   evidence.
//! - [`claim`] — durable work claims with compare-and-set revisions.
//! - [`effects`] — effects registered before they start, so a crash always
//!   leaves something to recover.
//! - [`cancel`] — cancellation that proves the turn is actually idle.
//! - [`journal`] — bounded scanning that counts what it could not read instead
//!   of skipping it in silence.
//! - [`sdk`] — a provider-neutral embeddable manager boundary with no raw
//!   transport and no self-asserted operator escape.
//!
//! Every module here is synchronous, allocation-bounded and free of I/O, so the
//! whole core is exhaustively testable offline. Nothing in it contacts a
//! provider, reads a credential, or opens a socket.

pub mod cancel;
pub mod claim;
pub mod effects;
pub mod journal;
pub mod observation;
pub mod progress;
pub mod retry;
pub mod sdk;
pub mod send;

pub use cancel::{CancelReason, CancelStatus, CancellationLedger, TurnIdleProof};
pub use claim::{ClaimError, ClaimLedger, ClaimRecord, Claimed, Revision};
pub use effects::{EffectKind, EffectRegistry, EffectState, EffectTicket, RecoveryReport};
pub use journal::{scan_ndjson, BoundedEventLog, Scan, ScanReport};
pub use observation::{
    BoundedProjection, RawObservation, RawObservationDigest, RefusalReason, TerminalObservation,
};
pub use progress::{ProgressLedger, RepeatClass, StopDecision, StopDetail};
pub use retry::{RetryBudget, RetryDecision, StandDownReason};
pub use sdk::{
    grant_operator_for_host, negotiate, BoundaryError, Capability, GrantProvenance, ManagerSession,
    NegotiationError, OperatorGrant, ProtocolVersion, RunProjection,
};
pub use send::{
    DeliveryKnowledge, SendAttempt, SendError, SendLedger, SendState, TransportEvidence,
};

#[cfg(test)]
mod tests {
    use super::sdk::{Capability, GrantProvenance, ManagerSession, OperatorGrant, ProtocolVersion};

    #[test]
    fn an_operator_grant_records_whether_it_is_canonical() {
        let provisional = OperatorGrant::issue(GrantProvenance::Provisional);
        assert!(!provisional.is_canonical());
        let canonical = OperatorGrant::issue(GrantProvenance::Canonical);
        assert!(canonical.is_canonical());
    }

    #[test]
    fn a_session_without_a_host_issued_grant_has_no_operator_authority() {
        let session = ManagerSession::open(ProtocolVersion::V2, [Capability::ReadRuns]);
        assert!(!session.has_operator_authority());
        assert!(session.require_operator().is_err());

        let elevated = ManagerSession::open(ProtocolVersion::V2, [Capability::ReadRuns])
            .with_operator(OperatorGrant::issue(GrantProvenance::Provisional));
        assert!(elevated.has_operator_authority());
        // Still not canonical: the grant says so, and a consumer must check.
        assert!(!elevated.require_operator().expect("granted").is_canonical());
    }
}
