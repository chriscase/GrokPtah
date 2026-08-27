//! Durable evidence for one provider request attempt, and the operator-driven
//! settlement of an attempt whose outcome was never observed.
//!
//! The existing send engine ([`crate::host`]'s run usage tracker) already
//! admits, counts, and closes provider attempts. It records *how many* are
//! outstanding, which is enough for token accounting and nothing else: after a
//! crash the count is zeroed and the fact that bytes may already have reached
//! the provider is gone. This module adds the missing durable fact — **was
//! this attempt dispatched, and did anyone ever see its outcome** — without
//! touching the provider wire, the request payload, or the retry machine.
//!
//! # States
//!
//! ```text
//!  Preparing ──dispatch──▶ Sent ──observe──▶ Resolved      (terminal)
//!      │                     │
//!   recover               recover
//!      │                     │
//!      ▼                     ▼
//!   NotSent (terminal)   Uncertain ──operator settlement──▶ Settled (terminal)
//! ```
//!
//! `NotSent` is the only recovery state that permits takeover: the attempt is
//! *known* never to have been dispatched, so the existing retry machine may
//! retry it. `Sent` and `Uncertain` refuse takeover — a retry there would be a
//! second, unaccounted provider request.
//!
//! Settlement records what an operator established out of band. It is a record
//! write only: it never re-sends, and it can never upgrade an attempt to
//! "successful", because nobody observed a response.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::authority::PrincipalScope;
use super::types::{OrchError, OrchErrorCode};

/// Schema version for durable provider-attempt records.
pub const PROVIDER_ATTEMPT_SCHEMA_VERSION: u32 = 1;

/// Upper bound on an operator's free-text settlement note. The note is only
/// ever read back by the credential that owns the attempt, so this bound only
/// has to stop an unbounded durable write.
pub const MAX_SETTLEMENT_NOTE_BYTES: usize = 2_000;

/// Where an attempt stands relative to the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptSendState {
    /// Admitted by the send engine; nothing has been written to the wire.
    Preparing,
    /// Handed to the transport. The outcome is not yet known.
    Sent,
    /// An outcome was observed — a response, or a transport failure that
    /// proves the provider never accepted the request.
    Resolved,
    /// Recovery proved the attempt was still `Preparing`, so it was never
    /// dispatched. Safe for the existing retry machine to take over.
    NotSent,
    /// Recovery found the attempt dispatched with no observed outcome. Only an
    /// operator with external evidence can close this.
    Uncertain,
    /// An operator settled an `Uncertain` attempt with positive evidence.
    Settled,
}

impl AttemptSendState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Preparing => "preparing",
            Self::Sent => "sent",
            Self::Resolved => "resolved",
            Self::NotSent => "not_sent",
            Self::Uncertain => "uncertain",
            Self::Settled => "settled",
        }
    }

    /// Terminal states need no further reconciliation.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Resolved | Self::NotSent | Self::Settled)
    }

    /// An attempt whose durable evidence may not be silently discarded.
    ///
    /// This is the retention predicate: anything not terminal is unsettled,
    /// and unsettled evidence outlives age and count limits.
    pub fn is_unsettled(self) -> bool {
        !self.is_terminal()
    }

    /// Whether the existing retry machine may retry this attempt's work.
    ///
    /// Only a provably undispatched attempt qualifies. Everything else — in
    /// particular `Sent` and `Uncertain` — must refuse, because a retry would
    /// duplicate a request that may already be in flight at the provider.
    pub fn permits_takeover(self) -> bool {
        matches!(self, Self::NotSent)
    }
}

/// What an operator established about a dispatched-but-unobserved attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettlementOutcome {
    /// Evidence shows the provider did receive and act on the request. The
    /// response content remains unobserved — this never implies success.
    Delivered,
    /// Evidence shows the request never reached the provider.
    NotDelivered,
}

impl SettlementOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::NotDelivered => "not_delivered",
        }
    }
}

/// Operator-supplied proof backing a settlement.
///
/// The digest is of the operator's *own* evidence artifact (a provider console
/// export, a billing line, a gateway log excerpt). GrokPtah never sees the
/// artifact and never reconstructs it, so this records that evidence existed
/// and pins which evidence it was — it is an audit anchor, not a verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettlementEvidence {
    /// Short operator-chosen classification, e.g. `provider_console_export`.
    pub kind: String,
    /// Lowercase hex SHA-256 of the operator's evidence artifact.
    pub digest: String,
    /// When the operator observed the evidence.
    pub observed_at: DateTime<Utc>,
}

impl SettlementEvidence {
    /// Reject absent, malformed, or non-canonical evidence.
    ///
    /// A settlement with no positive evidence is not a settlement, and a
    /// digest that is not exactly 64 lowercase hex characters is a forged or
    /// truncated digest rather than a SHA-256.
    pub fn validate(&self) -> Result<(), OrchError> {
        let kind = self.kind.trim();
        if kind.is_empty() || kind.len() > 64 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "settlement evidence kind must be 1-64 characters",
            ));
        }
        if !kind
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "settlement evidence kind must contain only ASCII letters, numbers, '-', '_', or '.'",
            ));
        }
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "settlement evidence digest must be 64 lowercase hex characters",
            ));
        }
        Ok(())
    }
}

/// The durable settlement stamped onto a reconciled attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptSettlement {
    pub outcome: SettlementOutcome,
    pub evidence: SettlementEvidence,
    /// Credential identity of the operator who authorized the settlement.
    pub operator_token_id: String,
    /// The reconciliation request that produced this settlement. Re-issuing
    /// the same request must not produce a second settlement.
    pub request_id: String,
    pub settled_at: DateTime<Utc>,
    /// Bounded operator note. Returned only to the credential the attempt is
    /// bound to, which is also the only credential permitted to settle it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One durable provider request attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAttempt {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Deterministic id: `{run_id}.attempt-{ordinal:06}`.
    pub attempt_id: String,
    pub run_id: String,
    /// One-based position of this attempt within its run.
    pub ordinal: u32,
    /// The orchestration request that created the owning run. Reconciliation
    /// requires the caller to restate it, so a caller holding only an
    /// attempt id cannot settle an attempt it did not originate.
    pub request_id: String,
    /// Authority binding of the caller whose operation produced this attempt.
    pub scope: PrincipalScope,
    /// Binds the attempt to (run, ordinal, request) without carrying any part
    /// of the provider request. See [`attempt_request_digest`].
    pub request_digest: String,
    pub send_state: AttemptSendState,
    /// Monotonic revision for compare-and-set. Every mutation increments it,
    /// and a settlement must name the revision it observed.
    pub revision: u64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// When the attempt was handed to the transport, if it ever was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatched_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<AttemptSettlement>,
}

fn default_schema_version() -> u32 {
    PROVIDER_ATTEMPT_SCHEMA_VERSION
}

/// Deterministic identity digest for an attempt.
///
/// Derived only from durable orchestration identifiers, never from the
/// provider request. Recomputing it at settlement time is what makes a forged
/// `requestDigest` detectable.
pub fn attempt_request_digest(run_id: &str, ordinal: u32, request_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    // Length-prefixed so ("ab", "c") and ("a", "bc") cannot collide.
    for field in [run_id, request_id] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hasher.update(ordinal.to_be_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Deterministic attempt id for a run ordinal.
pub fn attempt_id_for(run_id: &str, ordinal: u32) -> String {
    format!("{run_id}.attempt-{ordinal:06}")
}

impl ProviderAttempt {
    /// Open a new attempt in [`AttemptSendState::Preparing`].
    pub fn preparing(
        run_id: &str,
        ordinal: u32,
        request_id: &str,
        scope: PrincipalScope,
        now: DateTime<Utc>,
    ) -> Result<Self, OrchError> {
        if ordinal == 0 {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "provider attempt ordinal is one-based",
            ));
        }
        Ok(Self {
            schema_version: PROVIDER_ATTEMPT_SCHEMA_VERSION,
            attempt_id: attempt_id_for(run_id, ordinal),
            run_id: run_id.into(),
            ordinal,
            request_id: request_id.into(),
            scope,
            request_digest: attempt_request_digest(run_id, ordinal, request_id),
            send_state: AttemptSendState::Preparing,
            revision: 1,
            created_at: now,
            updated_at: now,
            dispatched_at: None,
            settlement: None,
        })
    }

    /// Whether the existing retry machine may retry this attempt's work.
    ///
    /// Stricter than [`AttemptSendState::permits_takeover`] because it also
    /// reads the settlement: an attempt an operator proved was never delivered
    /// is as safe to retry as one that was never dispatched, while an attempt
    /// settled as *delivered* stays off-limits forever — retrying it would
    /// duplicate a request the provider already acted on.
    pub fn permits_takeover(&self) -> bool {
        match self.send_state {
            AttemptSendState::NotSent => true,
            AttemptSendState::Settled => self
                .settlement
                .as_ref()
                .is_some_and(|settlement| settlement.outcome == SettlementOutcome::NotDelivered),
            _ => false,
        }
    }

    /// Whether this attempt's stored digest still matches its own identity.
    ///
    /// A record whose digest does not recompute has been tampered with on disk
    /// and must never be settled or counted as evidence.
    pub fn digest_is_intact(&self) -> bool {
        self.request_digest == attempt_request_digest(&self.run_id, self.ordinal, &self.request_id)
            && self.attempt_id == attempt_id_for(&self.run_id, self.ordinal)
    }

    fn advance(&mut self, next: AttemptSendState, now: DateTime<Utc>) {
        self.send_state = next;
        self.revision = self.revision.saturating_add(1);
        self.updated_at = now;
    }

    /// Mark the attempt dispatched. Only legal from `Preparing`.
    pub fn mark_sent(&mut self, now: DateTime<Utc>) -> Result<(), OrchError> {
        if self.send_state != AttemptSendState::Preparing {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!(
                    "provider attempt is {} and cannot be dispatched again",
                    self.send_state.as_str()
                ),
            ));
        }
        self.dispatched_at = Some(now);
        self.advance(AttemptSendState::Sent, now);
        Ok(())
    }

    /// Record that an outcome was observed. Legal from `Preparing` (the
    /// transport refused before dispatch) and from `Sent`.
    pub fn mark_resolved(&mut self, now: DateTime<Utc>) -> Result<(), OrchError> {
        match self.send_state {
            AttemptSendState::Preparing | AttemptSendState::Sent => {
                self.advance(AttemptSendState::Resolved, now);
                Ok(())
            }
            // Already resolved: repeating the observation is a no-op rather
            // than an error, so a retried teardown cannot fail a finished run.
            AttemptSendState::Resolved => Ok(()),
            other => Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!(
                    "provider attempt is {} and cannot be resolved",
                    other.as_str()
                ),
            )),
        }
    }

    /// Apply restart recovery. Returns `true` when the record changed.
    ///
    /// This is the fail-closed step: an attempt still `Preparing` is provably
    /// undispatched, while one left `Sent` becomes `Uncertain` and stays
    /// visible until an operator settles it.
    pub fn recover(&mut self, now: DateTime<Utc>) -> bool {
        match self.send_state {
            AttemptSendState::Preparing => {
                self.advance(AttemptSendState::NotSent, now);
                true
            }
            AttemptSendState::Sent => {
                self.advance(AttemptSendState::Uncertain, now);
                true
            }
            _ => false,
        }
    }

    /// Settle an uncertain attempt with operator evidence.
    ///
    /// Rejects a stale revision, a mismatched run/attempt/request binding, a
    /// tampered digest, malformed evidence, and any attempt to settle
    /// something that is not actually uncertain. It performs no provider I/O:
    /// a settlement is a record write, never a resend.
    pub fn settle(
        &mut self,
        binding: &SettlementBinding<'_>,
        expected_revision: u64,
        outcome: SettlementOutcome,
        evidence: SettlementEvidence,
        note: Option<String>,
        now: DateTime<Utc>,
    ) -> Result<(), OrchError> {
        if !self.digest_is_intact() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "provider attempt identity digest does not match its record",
            ));
        }
        binding.verify(self)?;
        evidence.validate()?;
        if let Some(note) = note.as_deref() {
            if note.len() > MAX_SETTLEMENT_NOTE_BYTES {
                return Err(OrchError::new(
                    OrchErrorCode::InvalidRequest,
                    format!("settlement note exceeds {MAX_SETTLEMENT_NOTE_BYTES} bytes"),
                ));
            }
        }
        if self.revision != expected_revision {
            return Err(OrchError::new(
                OrchErrorCode::StaleVersion,
                "provider attempt revision changed; re-read the attempt and retry",
            ));
        }
        if self.send_state != AttemptSendState::Uncertain {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                format!(
                    "only an uncertain provider attempt can be settled; this one is {}",
                    self.send_state.as_str()
                ),
            ));
        }
        self.settlement = Some(AttemptSettlement {
            outcome,
            evidence,
            operator_token_id: binding.operator_token_id.into(),
            request_id: binding.reconcile_request_id.into(),
            settled_at: now,
            note,
        });
        self.advance(AttemptSendState::Settled, now);
        Ok(())
    }
}

/// The exact binding a settlement must restate to be accepted.
pub struct SettlementBinding<'a> {
    pub run_id: &'a str,
    pub attempt_id: &'a str,
    /// The originating run's request id, restated by the operator.
    pub request_id: &'a str,
    /// The reconciliation operation's own request id, used for idempotency.
    pub reconcile_request_id: &'a str,
    pub operator_token_id: &'a str,
}

impl SettlementBinding<'_> {
    fn verify(&self, attempt: &ProviderAttempt) -> Result<(), OrchError> {
        let matches = self.run_id == attempt.run_id
            && self.attempt_id == attempt.attempt_id
            && self.request_id == attempt.request_id;
        if !matches {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "reconciliation must name the exact run, attempt, and request of the attempt",
            ));
        }
        if self.reconcile_request_id.trim().is_empty() {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "reconciliation request_id must not be empty",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> PrincipalScope {
        PrincipalScope {
            owner_id: "account-1".into(),
            token_id: "laptop".into(),
            session_id: None,
            workspace: None,
        }
    }

    fn evidence() -> SettlementEvidence {
        SettlementEvidence {
            kind: "provider_console_export".into(),
            digest: "a".repeat(64),
            observed_at: Utc::now(),
        }
    }

    fn uncertain() -> ProviderAttempt {
        let mut attempt =
            ProviderAttempt::preparing("run-1", 1, "req-1", scope(), Utc::now()).unwrap();
        attempt.mark_sent(Utc::now()).unwrap();
        assert!(attempt.recover(Utc::now()));
        assert_eq!(attempt.send_state, AttemptSendState::Uncertain);
        attempt
    }

    fn binding<'a>(attempt: &'a ProviderAttempt, reconcile: &'a str) -> SettlementBinding<'a> {
        SettlementBinding {
            run_id: &attempt.run_id,
            attempt_id: &attempt.attempt_id,
            request_id: &attempt.request_id,
            reconcile_request_id: reconcile,
            operator_token_id: "laptop",
        }
    }

    #[test]
    fn recovery_separates_known_not_sent_from_uncertain() {
        let mut never =
            ProviderAttempt::preparing("run-1", 1, "req-1", scope(), Utc::now()).unwrap();
        assert!(never.recover(Utc::now()));
        assert_eq!(never.send_state, AttemptSendState::NotSent);
        assert!(never.send_state.permits_takeover());
        assert!(!never.send_state.is_unsettled());

        let dispatched = uncertain();
        assert!(!dispatched.send_state.permits_takeover());
        assert!(dispatched.send_state.is_unsettled());
    }

    #[test]
    fn recovery_is_idempotent_across_repeated_restarts() {
        let mut attempt = uncertain();
        let revision = attempt.revision;
        assert!(!attempt.recover(Utc::now()));
        assert_eq!(attempt.revision, revision);
        assert_eq!(attempt.send_state, AttemptSendState::Uncertain);
    }

    #[test]
    fn settlement_requires_exact_binding_and_current_revision() {
        let mut attempt = uncertain();
        let revision = attempt.revision;

        let wrong_request = SettlementBinding {
            run_id: "run-1",
            attempt_id: &attempt.attempt_id.clone(),
            request_id: "req-other",
            reconcile_request_id: "rec-1",
            operator_token_id: "laptop",
        };
        assert!(attempt
            .settle(
                &wrong_request,
                revision,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now()
            )
            .is_err());

        let bound = binding(&attempt, "rec-1");
        let stale = attempt
            .clone()
            .settle(
                &bound,
                revision - 1,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert_eq!(stale.code, OrchErrorCode::StaleVersion);

        let bound = SettlementBinding {
            run_id: "run-1",
            attempt_id: "run-1.attempt-000001",
            request_id: "req-1",
            reconcile_request_id: "rec-1",
            operator_token_id: "laptop",
        };
        attempt
            .settle(
                &bound,
                revision,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap();
        assert_eq!(attempt.send_state, AttemptSendState::Settled);
        assert!(attempt.send_state.is_terminal());
        assert!(!attempt.send_state.permits_takeover());
    }

    #[test]
    fn settled_attempt_cannot_be_settled_again() {
        let mut attempt = uncertain();
        let revision = attempt.revision;
        let bound = SettlementBinding {
            run_id: "run-1",
            attempt_id: "run-1.attempt-000001",
            request_id: "req-1",
            reconcile_request_id: "rec-1",
            operator_token_id: "laptop",
        };
        attempt
            .settle(
                &bound,
                revision,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap();
        let next = attempt.revision;
        let error = attempt
            .settle(
                &bound,
                next,
                SettlementOutcome::NotDelivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
    }

    #[test]
    fn forged_digest_blocks_settlement() {
        let mut attempt = uncertain();
        attempt.request_digest = "b".repeat(64);
        let revision = attempt.revision;
        let bound = SettlementBinding {
            run_id: "run-1",
            attempt_id: "run-1.attempt-000001",
            request_id: "req-1",
            reconcile_request_id: "rec-1",
            operator_token_id: "laptop",
        };
        let error = attempt
            .settle(
                &bound,
                revision,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);
    }

    #[test]
    fn malformed_evidence_is_rejected() {
        for bad in [
            SettlementEvidence {
                kind: String::new(),
                digest: "a".repeat(64),
                observed_at: Utc::now(),
            },
            SettlementEvidence {
                kind: "console".into(),
                digest: "a".repeat(63),
                observed_at: Utc::now(),
            },
            SettlementEvidence {
                kind: "console".into(),
                // Uppercase hex is not the canonical form.
                digest: "A".repeat(64),
                observed_at: Utc::now(),
            },
            SettlementEvidence {
                kind: "console/../etc".into(),
                digest: "a".repeat(64),
                observed_at: Utc::now(),
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?} must be rejected");
        }
        assert!(evidence().validate().is_ok());
    }

    #[test]
    fn digest_is_length_prefixed_against_field_collisions() {
        assert_ne!(
            attempt_request_digest("ab", 1, "c"),
            attempt_request_digest("a", 1, "bc")
        );
        assert_ne!(
            attempt_request_digest("run", 1, "req"),
            attempt_request_digest("run", 2, "req")
        );
    }

    #[test]
    fn takeover_follows_the_settlement_not_just_the_state() {
        let mut delivered = uncertain();
        let revision = delivered.revision;
        let bound = SettlementBinding {
            run_id: "run-1",
            attempt_id: "run-1.attempt-000001",
            request_id: "req-1",
            reconcile_request_id: "rec-1",
            operator_token_id: "laptop",
        };
        delivered
            .settle(
                &bound,
                revision,
                SettlementOutcome::Delivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap();
        assert!(
            !delivered.permits_takeover(),
            "an attempt proven delivered must never be retried"
        );

        let mut not_delivered = uncertain();
        let revision = not_delivered.revision;
        not_delivered
            .settle(
                &bound,
                revision,
                SettlementOutcome::NotDelivered,
                evidence(),
                None,
                Utc::now(),
            )
            .unwrap();
        assert!(
            not_delivered.permits_takeover(),
            "an attempt proven undelivered is as safe to retry as one never sent"
        );

        let mut never =
            ProviderAttempt::preparing("run-2", 1, "req-2", scope(), Utc::now()).unwrap();
        never.recover(Utc::now());
        assert!(never.permits_takeover());
        assert!(!uncertain().permits_takeover());
    }

    #[test]
    fn resolved_attempt_never_becomes_uncertain_on_restart() {
        let mut attempt =
            ProviderAttempt::preparing("run-1", 1, "req-1", scope(), Utc::now()).unwrap();
        attempt.mark_sent(Utc::now()).unwrap();
        attempt.mark_resolved(Utc::now()).unwrap();
        assert!(!attempt.recover(Utc::now()));
        assert_eq!(attempt.send_state, AttemptSendState::Resolved);
    }

    #[test]
    fn dispatch_is_not_repeatable() {
        let mut attempt =
            ProviderAttempt::preparing("run-1", 1, "req-1", scope(), Utc::now()).unwrap();
        attempt.mark_sent(Utc::now()).unwrap();
        assert!(attempt.mark_sent(Utc::now()).is_err());
    }
}
