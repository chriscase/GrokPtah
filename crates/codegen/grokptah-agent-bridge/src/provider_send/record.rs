//! The durable provider-attempt record and its single settlement bundle (#478).
//!
//! There is exactly one durable record per physical attempt, and settlement,
//! cancellation, provider receipt, run/token accounting, and the canonical audit
//! outcome live inside one value written by one atomic rename. They cannot split
//! into contradictory durable states because there is no interleaving in which
//! only some of them reach the disk.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::identity::{AttemptBinding, OpaqueId};
use super::state::{
    DeliveryKnowledge, HostEvidence, ProviderAttemptState, TransportEvidence, UncertaintyClass,
};

/// Schema version of the durable record. A restart refuses to interpret a
/// record from a version it does not understand rather than guessing.
pub const PROVIDER_ATTEMPT_SCHEMA_VERSION: u32 = 1;

/// Bounded transition history. Long enough to explain an attempt, short enough
/// that a pathological retry loop cannot grow a record without limit.
pub const MAX_RETAINED_TRANSITIONS: usize = 32;

/// Identity of one running host process. Used to tell "my own in-flight
/// attempt" from "an attempt orphaned by a dead process".
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostIncarnationId(String);

impl HostIncarnationId {
    pub fn new_random() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    pub fn from_raw(value: &str) -> Self {
        Self(value.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One durable state change with the evidence that justified it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptTransition {
    pub to: ProviderAttemptState,
    pub at: DateTime<Utc>,
    pub evidence: TransitionEvidence,
}

/// Why a transition was allowed. Every transition carries one; there is no
/// "because the caller said so" variant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
pub enum TransitionEvidence {
    /// The host created the durable intent.
    Intent,
    /// The host is about to create the send future.
    PreDispatch,
    /// The host proved no byte moved.
    Host(HostEvidence),
    /// The transport reported what actually happened.
    Transport(TransportEvidence),
    /// An explicit out-of-band reconciliation grant (#466) resolved an
    /// uncertain attempt. Never produced by provider I/O in this crate.
    ReconciliationGrant {
        grant_id: OpaqueId,
        grant_version: u32,
    },
}

/// What the attempt finally amounted to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementOutcome {
    /// The provider produced a complete, successful response.
    Completed,
    /// The provider answered and explicitly rejected the request.
    ProviderRejected,
    /// The request provably never reached the provider.
    NotSent,
    /// A write may have happened; the result is unknown.
    Uncertain,
}

impl SettlementOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ProviderRejected => "provider_rejected",
            Self::NotSent => "not_sent",
            Self::Uncertain => "uncertain",
        }
    }
}

/// Whether and when cancellation was involved.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CancellationRecord {
    #[default]
    NotRequested,
    /// Cancellation was observed strictly before the send future existed.
    RequestedBeforeDispatch,
    /// Cancellation was observed after the send future existed, so delivery is
    /// not decided by the cancellation itself.
    RequestedAfterDispatch,
}

/// The provider's own identity for the request, kept strictly distinct from the
/// host idempotency key and stored opaque.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRecord {
    /// Present only when the provider actually returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_receipt: Option<OpaqueId>,
    /// HTTP status the provider settled with, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

/// Run and token accounting for the attempt.
///
/// Absent usage is `None`, never zero: "the provider reported no usage" and
/// "the provider reported zero tokens" are different facts and the projection
/// has to be able to tell them apart.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AccountingRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    /// Request bytes the host was prepared to write.
    pub request_bytes: u64,
    /// Response bytes actually observed.
    pub response_bytes: u64,
}

/// The canonical audit outcome for this attempt (#462 seam).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// The attempt is fully accounted for and audit can close it.
    Accounted,
    /// The attempt is on record as unresolved and audit must keep it open.
    Unresolved,
}

/// Settlement, cancellation, receipt, accounting, and audit outcome as one
/// indivisible value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settlement {
    pub outcome: SettlementOutcome,
    pub cancellation: CancellationRecord,
    pub receipt: ReceiptRecord,
    pub accounting: AccountingRecord,
    pub audit: AuditOutcome,
    pub settled_at: DateTime<Utc>,
    /// Present only for `Uncertain`; explains what is unknown, without leaking
    /// transport diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertainty: Option<UncertaintyClass>,
}

/// A settlement that contradicts itself is rejected before it can be written.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementContradiction {
    /// A provably un-sent attempt cannot carry a provider receipt or status.
    NotSentWithReceipt,
    /// A provably un-sent attempt cannot have consumed provider tokens.
    NotSentWithUsage,
    /// A provably un-sent attempt cannot have received response bytes.
    NotSentWithResponseBytes,
    /// An unresolved attempt cannot be closed by audit.
    UncertainButAccounted,
    /// A resolved attempt cannot be left open by audit.
    ResolvedButUnresolvedAudit,
    /// Only an uncertain settlement carries an uncertainty class.
    UncertaintyClassOnResolvedOutcome,
    /// An uncertain settlement must say what is unknown.
    UncertainWithoutClass,
    /// A completed attempt must have observed a successful status.
    CompletedWithoutSuccessStatus,
    /// Cancellation before dispatch cannot coexist with a delivered outcome.
    PreDispatchCancellationWithDelivery,
}

impl std::fmt::Display for SettlementContradiction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::NotSentWithReceipt => "not-sent settlement carries a provider receipt",
            Self::NotSentWithUsage => "not-sent settlement reports token usage",
            Self::NotSentWithResponseBytes => "not-sent settlement reports response bytes",
            Self::UncertainButAccounted => "uncertain settlement is marked audit-accounted",
            Self::ResolvedButUnresolvedAudit => "resolved settlement is marked audit-unresolved",
            Self::UncertaintyClassOnResolvedOutcome => {
                "resolved settlement carries an uncertainty class"
            }
            Self::UncertainWithoutClass => "uncertain settlement has no uncertainty class",
            Self::CompletedWithoutSuccessStatus => {
                "completed settlement has no successful provider status"
            }
            Self::PreDispatchCancellationWithDelivery => {
                "pre-dispatch cancellation contradicts a delivered outcome"
            }
        };
        f.write_str(text)
    }
}

impl std::error::Error for SettlementContradiction {}

impl Settlement {
    /// Reject every combination in which the five facts disagree.
    pub fn validate(&self) -> Result<(), SettlementContradiction> {
        use SettlementContradiction as C;
        let usage_reported =
            self.accounting.prompt_tokens.is_some() || self.accounting.completion_tokens.is_some();
        match self.outcome {
            SettlementOutcome::NotSent => {
                if self.receipt.provider_receipt.is_some() || self.receipt.status.is_some() {
                    return Err(C::NotSentWithReceipt);
                }
                if usage_reported {
                    return Err(C::NotSentWithUsage);
                }
                if self.accounting.response_bytes != 0 {
                    return Err(C::NotSentWithResponseBytes);
                }
            }
            SettlementOutcome::Completed => {
                match self.receipt.status {
                    Some(status) if (200..300).contains(&status) => {}
                    _ => return Err(C::CompletedWithoutSuccessStatus),
                }
                if self.cancellation == CancellationRecord::RequestedBeforeDispatch {
                    return Err(C::PreDispatchCancellationWithDelivery);
                }
            }
            SettlementOutcome::ProviderRejected => {
                if self.cancellation == CancellationRecord::RequestedBeforeDispatch {
                    return Err(C::PreDispatchCancellationWithDelivery);
                }
            }
            SettlementOutcome::Uncertain => {}
        }
        match (self.outcome, self.audit) {
            (SettlementOutcome::Uncertain, AuditOutcome::Accounted) => {
                return Err(C::UncertainButAccounted)
            }
            (SettlementOutcome::Uncertain, AuditOutcome::Unresolved) => {}
            (_, AuditOutcome::Unresolved) => return Err(C::ResolvedButUnresolvedAudit),
            (_, AuditOutcome::Accounted) => {}
        }
        match (self.outcome, self.uncertainty) {
            (SettlementOutcome::Uncertain, None) => return Err(C::UncertainWithoutClass),
            (SettlementOutcome::Uncertain, Some(_)) => {}
            (_, Some(_)) => return Err(C::UncertaintyClassOnResolvedOutcome),
            (_, None) => {}
        }
        Ok(())
    }
}

/// The one durable provider-attempt record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttempt {
    pub schema_version: u32,
    /// Opaque id, stable for the life of the attempt.
    pub attempt_id: String,
    /// Compare-and-swap revision. Every durable mutation increments it.
    pub revision: u64,
    pub binding: AttemptBinding,
    pub state: ProviderAttemptState,
    /// The host incarnation that currently owns the attempt.
    pub owner: HostIncarnationId,
    pub created_at: DateTime<Utc>,
    pub state_changed_at: DateTime<Utc>,
    pub transitions: Vec<AttemptTransition>,
    /// Present exactly once the attempt has settled in the lattice sense.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<Settlement>,
}

impl ProviderAttempt {
    pub(crate) fn new(
        binding: AttemptBinding,
        owner: HostIncarnationId,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: PROVIDER_ATTEMPT_SCHEMA_VERSION,
            attempt_id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            binding,
            state: ProviderAttemptState::Preparing,
            owner,
            created_at: now,
            state_changed_at: now,
            transitions: vec![AttemptTransition {
                to: ProviderAttemptState::Preparing,
                at: now,
                evidence: TransitionEvidence::Intent,
            }],
            settlement: None,
        }
    }

    pub fn ordinal(&self) -> u64 {
        self.binding.ordinal()
    }

    pub fn delivery_knowledge(&self) -> DeliveryKnowledge {
        self.state.delivery_knowledge()
    }

    /// The single retry rule, applied to a durable record.
    pub fn may_auto_retry(&self) -> bool {
        self.state.may_auto_retry()
    }

    /// Whether this record blocks a new ordinal in its scope. An attempt whose
    /// delivery is unknown must be resolved, not stepped over.
    pub fn blocks_new_ordinal(&self) -> bool {
        !self.state.is_terminal()
    }

    pub(crate) fn push_transition(
        &mut self,
        to: ProviderAttemptState,
        evidence: TransitionEvidence,
        now: DateTime<Utc>,
    ) {
        self.state = to;
        self.state_changed_at = now;
        self.revision = self.revision.saturating_add(1);
        self.transitions.push(AttemptTransition {
            to,
            at: now,
            evidence,
        });
        if self.transitions.len() > MAX_RETAINED_TRANSITIONS {
            // Keep the first transition (the intent) and the most recent tail:
            // the beginning and the end are what explain an attempt.
            let keep_tail = MAX_RETAINED_TRANSITIONS - 1;
            let tail_start = self.transitions.len() - keep_tail;
            let mut kept = Vec::with_capacity(MAX_RETAINED_TRANSITIONS);
            kept.push(self.transitions[0].clone());
            kept.extend_from_slice(&self.transitions[tail_start..]);
            self.transitions = kept;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_send::identity::fixtures;

    fn settlement(outcome: SettlementOutcome) -> Settlement {
        Settlement {
            outcome,
            cancellation: CancellationRecord::NotRequested,
            receipt: ReceiptRecord {
                provider_receipt: None,
                status: match outcome {
                    SettlementOutcome::Completed => Some(200),
                    SettlementOutcome::ProviderRejected => Some(429),
                    _ => None,
                },
            },
            accounting: AccountingRecord {
                request_bytes: 10,
                ..AccountingRecord::default()
            },
            audit: match outcome {
                SettlementOutcome::Uncertain => AuditOutcome::Unresolved,
                _ => AuditOutcome::Accounted,
            },
            settled_at: Utc::now(),
            uncertainty: match outcome {
                SettlementOutcome::Uncertain => Some(UncertaintyClass::Timeout),
                _ => None,
            },
        }
    }

    #[test]
    fn consistent_settlements_validate() {
        for outcome in [
            SettlementOutcome::Completed,
            SettlementOutcome::ProviderRejected,
            SettlementOutcome::NotSent,
            SettlementOutcome::Uncertain,
        ] {
            settlement(outcome)
                .validate()
                .unwrap_or_else(|error| panic!("{outcome:?} should validate: {error}"));
        }
    }

    #[test]
    fn a_not_sent_attempt_cannot_carry_a_receipt() {
        let mut value = settlement(SettlementOutcome::NotSent);
        value.receipt.status = Some(200);
        assert_eq!(
            value.validate(),
            Err(SettlementContradiction::NotSentWithReceipt)
        );
    }

    #[test]
    fn a_not_sent_attempt_cannot_report_tokens_or_response_bytes() {
        let mut usage = settlement(SettlementOutcome::NotSent);
        usage.accounting.completion_tokens = Some(1);
        assert_eq!(
            usage.validate(),
            Err(SettlementContradiction::NotSentWithUsage)
        );

        let mut bytes = settlement(SettlementOutcome::NotSent);
        bytes.accounting.response_bytes = 1;
        assert_eq!(
            bytes.validate(),
            Err(SettlementContradiction::NotSentWithResponseBytes)
        );
    }

    #[test]
    fn audit_cannot_close_an_uncertain_attempt() {
        let mut value = settlement(SettlementOutcome::Uncertain);
        value.audit = AuditOutcome::Accounted;
        assert_eq!(
            value.validate(),
            Err(SettlementContradiction::UncertainButAccounted)
        );
    }

    #[test]
    fn audit_cannot_leave_a_resolved_attempt_open() {
        let mut value = settlement(SettlementOutcome::Completed);
        value.audit = AuditOutcome::Unresolved;
        assert_eq!(
            value.validate(),
            Err(SettlementContradiction::ResolvedButUnresolvedAudit)
        );
    }

    #[test]
    fn a_completed_settlement_needs_a_successful_status() {
        let mut value = settlement(SettlementOutcome::Completed);
        value.receipt.status = Some(500);
        assert_eq!(
            value.validate(),
            Err(SettlementContradiction::CompletedWithoutSuccessStatus)
        );
    }

    #[test]
    fn cancellation_before_dispatch_cannot_coexist_with_delivery() {
        let mut value = settlement(SettlementOutcome::Completed);
        value.cancellation = CancellationRecord::RequestedBeforeDispatch;
        assert_eq!(
            value.validate(),
            Err(SettlementContradiction::PreDispatchCancellationWithDelivery)
        );
    }

    #[test]
    fn transition_history_stays_bounded_and_keeps_the_intent() {
        let binding =
            crate::provider_send::identity::AttemptBinding::seal(fixtures::spec("s", "b"), 1);
        let mut attempt =
            ProviderAttempt::new(binding, HostIncarnationId::new_random(), Utc::now());
        for _ in 0..(MAX_RETAINED_TRANSITIONS * 3) {
            attempt.push_transition(
                ProviderAttemptState::Sending,
                TransitionEvidence::PreDispatch,
                Utc::now(),
            );
        }
        assert_eq!(attempt.transitions.len(), MAX_RETAINED_TRANSITIONS);
        assert_eq!(attempt.transitions[0].to, ProviderAttemptState::Preparing);
        assert!(attempt.revision > MAX_RETAINED_TRANSITIONS as u64);
    }
}
