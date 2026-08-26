//! One provider attempt: what it was bound to, and whether it was sent.
//!
//! [`crate::launch`] answers *may* a run start. This module answers the
//! question that follows it and is far more expensive to get wrong: **did the
//! request actually reach the provider, and is it safe to send it again?**
//!
//! A retry that duplicates work already performed is not a recovery, it is a
//! second charge and a second set of side effects. So the send boundary is
//! recorded as an explicit, monotonic state:
//!
//! ```text
//! KnownNotSent ──► Sending ──► Sent
//!                        └───► Uncertain
//! ```
//!
//! Only [`SendState::KnownNotSent`] may be retried automatically. `Sending`
//! and `Uncertain` require a human or an explicit provider-side reconciliation
//! against the recorded idempotency key; `Sent` is finished. A process that
//! dies mid-send leaves `Sending` on disk, which is deliberately *not*
//! auto-retryable: an interrupted send is indistinguishable from a delivered
//! one without asking the provider.
//!
//! # What is bound, and why
//!
//! Every field in [`AttemptBinding`] is something that, if it changed between
//! deciding and sending, would mean the request went somewhere the operator
//! did not authorize: a different tenant, a different workspace, a different
//! model, a stale policy or capability decision. Binding them into the record
//! makes a drift detectable after the fact rather than only preventable
//! before it.
//!
//! # Non-goals (enforced structurally)
//!
//! No field can carry a bearer, refresh token, API key, keychain reference,
//! endpoint URL, hostname, prompt text, or response body. The provider
//! receipts are opaque provider-assigned identifiers, charset- and
//! length-bounded, and [`UsageReceipt`] carries counts only.
//!
//! Nothing here is an entitlement, quota, balance, or billing statement, and
//! nothing here is derived from a local clock reading. A local timestamp is
//! evidence about *this host's* record-keeping, never about whether a token
//! is spendable — only the provider can say that.

use serde::{Deserialize, Serialize};

use crate::account::{AccountReference, CredentialMethod};
use crate::launch::{BaseCategory, ModelReference, ProviderClass, RequestDialect, RouteClass};
use crate::outcome::{RunFailureKind, TerminalVerdict};

/// Stable contract identifier for the provider attempt record.
pub const GROK_ATTEMPT_CONTRACT_VERSION: &str = "grokptah.attempt.v1";
/// Numeric schema revision carried in every attempt record.
pub const GROK_ATTEMPT_SCHEMA_VERSION: u32 = 1;
/// Maximum UTF-8 bytes accepted in any bounded identifier here.
pub const MAX_ATTEMPT_IDENTIFIER_BYTES: usize = 128;

/// A bounded, non-secret identifier.
///
/// Used for every identity and receipt in this module so no unbounded,
/// caller- or provider-controlled text reaches a durable record, a UI, or an
/// accessibility tree. The charset excludes whitespace, control characters,
/// and markup, and traversal-shaped values are refused outright.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BoundedId(String);

impl BoundedId {
    /// Accept an identifier only when it is safe to record verbatim.
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        if value.is_empty() || value.len() > MAX_ATTEMPT_IDENTIFIER_BYTES {
            return None;
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        }) {
            return None;
        }
        if value.contains("..") {
            return None;
        }
        let edge = |byte: u8| matches!(byte, b'/' | b'.' | b':' | b'-' | b'_');
        if value.bytes().next().is_some_and(edge) || value.bytes().next_back().is_some_and(edge) {
            return None;
        }
        Some(Self(value.to_string()))
    }

    /// The bounded value.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this identifier still satisfies its own bounds.
    ///
    /// A decoded record can carry anything the file contained, so every
    /// validator re-checks rather than trusting the type.
    pub fn is_bounded(&self) -> bool {
        Self::new(&self.0).as_ref() == Some(self)
    }
}

/// A monotonically increasing revision of some authority decision.
///
/// Recorded rather than re-derived: a policy or capability decision that has
/// been superseded must be detectable as stale even after the decision itself
/// is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub u64);

/// Every authority revision an attempt was decided under.
///
/// A change in any of these between deciding and sending means the request
/// would go out under an authority the operator never approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthorityRevisions {
    /// Revision of the authentication decision (who this host is acting as).
    pub auth: Revision,
    /// Revision of the policy decision (what that principal may do).
    pub policy: Revision,
    /// Revision of the capability decision (what this model/provider offers).
    pub capability: Revision,
    /// Revision of the credential material itself (rotation counter).
    pub credential: Revision,
}

/// Who and where an attempt is acting for.
///
/// All identifiers are opaque and bounded. Display names, email addresses,
/// and filesystem paths are deliberately absent: they are personal data or
/// host detail, and an opaque durable identifier already disambiguates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptSubject {
    /// The acting principal, when the route publishes a durable identity.
    ///
    /// `None` is itself a bound fact: a bare API-key route carries no durable
    /// account identity, and recording "none published" is honest where
    /// synthesising one would not be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub principal: Option<BoundedId>,
    /// The tenant that principal is acting within, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<BoundedId>,
    /// The project this attempt belongs to, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<BoundedId>,
    /// The approved workspace identity. An opaque handle, never a path.
    pub workspace: BoundedId,
    /// The session this attempt belongs to.
    pub session: BoundedId,
}

impl AttemptSubject {
    /// Whether every identifier is still within bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        let bounded = self.principal.as_ref().is_none_or(BoundedId::is_bounded)
            && self.workspace.is_bounded()
            && self.session.is_bounded()
            && self.tenant.as_ref().is_none_or(BoundedId::is_bounded)
            && self.project.as_ref().is_none_or(BoundedId::is_bounded);
        if bounded {
            Ok(())
        } else {
            Err("attempt subject carries an identifier that is not bounded and opaque")
        }
    }
}

/// The exact route an attempt is bound to.
///
/// Reuses [`crate::launch`]'s closed vocabularies so the facts a run was
/// admitted on and the facts an attempt was sent under are literally the same
/// types, and a drift between them is a type-level comparison rather than a
/// string match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptRoute {
    /// Provider family.
    pub provider: ProviderClass,
    /// Bounded provider profile identity, when one was selected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<BoundedId>,
    /// Credential route.
    pub credential_method: CredentialMethod,
    /// Request route.
    pub route: RouteClass,
    /// Base endpoint category. Never carries a URL.
    pub base: BaseCategory,
    /// Request dialect.
    pub dialect: RequestDialect,
    /// Selected model.
    pub model: ModelReference,
    /// Selected reasoning effort, as its exact wire value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<BoundedId>,
    /// Bounded account handle this attempt bills against, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_reference: Option<AccountReference>,
}

impl AttemptRoute {
    /// Whether every bounded part of this route is still within bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        if ModelReference::new(&self.model.value).as_ref() != Some(&self.model) {
            return Err("attempt model is not a bounded opaque identifier");
        }
        if !self.profile.as_ref().is_none_or(BoundedId::is_bounded) {
            return Err("attempt profile is not a bounded opaque identifier");
        }
        if !self.effort.as_ref().is_none_or(BoundedId::is_bounded) {
            return Err("attempt effort is not a bounded opaque value");
        }
        match &self.account_reference {
            Some(reference)
                if AccountReference::new(&reference.value, reference.source).as_ref()
                    != Some(reference) =>
            {
                Err("attempt account reference is not a bounded opaque identifier")
            }
            _ => Ok(()),
        }
    }
}

/// The immutable statement of what an attempt was for.
///
/// A digest rather than the prompt itself: the intent must be comparable
/// across a retry without a durable record holding user text. The digest is
/// computed by the host and is opaque here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AttemptIntent {
    /// Opaque digest of the exact request this attempt carries.
    pub digest: BoundedId,
    /// Caller idempotency key for the originating intent.
    pub request_id: BoundedId,
    /// The idempotency key presented to the provider.
    ///
    /// Recorded so an [`SendState::Uncertain`] attempt can be reconciled
    /// against the provider rather than blindly repeated.
    pub provider_idempotency_key: BoundedId,
}

impl AttemptIntent {
    /// Whether every identifier is still within bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.digest.is_bounded()
            && self.request_id.is_bounded()
            && self.provider_idempotency_key.is_bounded()
        {
            Ok(())
        } else {
            Err("attempt intent carries an identifier that is not bounded and opaque")
        }
    }
}

/// Opaque provider-assigned identifiers for one dispatched request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderReceipts {
    /// The provider's request identifier, when it returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<BoundedId>,
    /// The provider's run/response identifier, when it returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<BoundedId>,
    /// Token counts the provider reported for this attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<UsageReceipt>,
    /// Whether a complete, parseable reply was received from the provider.
    ///
    /// Separate from the identifiers above because acknowledgement and
    /// identification are different facts: a provider that answers correctly
    /// but returns no request or run id has still unambiguously received the
    /// request, and calling that `uncertain` would block retries forever on a
    /// perfectly healthy route.
    #[serde(default, skip_serializing_if = "is_false")]
    pub provider_replied: bool,
}

const fn is_false(value: &bool) -> bool {
    !*value
}

impl ProviderReceipts {
    /// Whether every recorded receipt is still within bounds.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.request.as_ref().is_none_or(BoundedId::is_bounded)
            && self.run.as_ref().is_none_or(BoundedId::is_bounded)
        {
            Ok(())
        } else {
            Err("provider receipt is not a bounded opaque identifier")
        }
    }

    /// Whether the provider acknowledged this attempt at all.
    pub fn acknowledged(&self) -> bool {
        self.provider_replied
            || self.request.is_some()
            || self.run.is_some()
            || self.usage.is_some()
    }
}

/// Token counts the provider reported.
///
/// Counts only. This is a record of what was consumed, never a statement
/// about entitlement, remaining quota, balance, or cost: none of those can be
/// established without a provider round-trip this record does not perform.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageReceipt {
    /// Prompt tokens the provider reported.
    pub input_tokens: u64,
    /// Completion tokens the provider reported.
    pub output_tokens: u64,
}

/// Whether a request reached the provider.
///
/// The whole point of this enum is [`SendState::Uncertain`]: without it a host
/// must choose between never retrying (losing recoverable work) and always
/// retrying (duplicating charges and side effects). Naming the ambiguity lets
/// the safe rule be stated exactly once, in [`SendState::may_auto_retry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendState {
    /// The request was prepared but has provably not left this host.
    KnownNotSent,
    /// The request is in flight. Whether it arrived is not yet known.
    Sending,
    /// The provider acknowledged the request.
    Sent,
    /// The outcome is unknown: the connection broke, the process died, or the
    /// reply could not be parsed. The request may or may not have run.
    Uncertain,
    /// The provider acknowledged the request and a response is being consumed.
    Responding,
    /// Terminal for this identity. Capacity may be released only after this.
    Settled,
}

impl SendState {
    /// The exact wire value used by the JSON contract.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::KnownNotSent => "known_not_sent",
            Self::Sending => "sending",
            Self::Sent => "sent",
            Self::Uncertain => "uncertain",
            Self::Responding => "responding",
            Self::Settled => "settled",
        }
    }

    /// Every state in declaration order, for schema and parity pinning.
    pub const ALL: [Self; 6] = [
        Self::KnownNotSent,
        Self::Sending,
        Self::Sent,
        Self::Uncertain,
        Self::Responding,
        Self::Settled,
    ];

    /// Whether the host may re-send this attempt without asking anyone.
    ///
    /// Only a request that provably never left. An interrupted `Sending` is
    /// indistinguishable from a delivered one without asking the provider, so
    /// it is not auto-retryable however tempting that is.
    pub const fn may_auto_retry(self) -> bool {
        matches!(self, Self::KnownNotSent)
    }

    /// Whether this attempt is finished, successfully or not.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Settled)
    }

    /// Whether this attempt must be reconciled against the provider before
    /// any equivalent request is issued again.
    pub const fn requires_reconciliation(self) -> bool {
        matches!(self, Self::Sending | Self::Uncertain | Self::Responding)
    }

    /// Whether `next` is a legal successor of `self`.
    ///
    /// The lattice is strictly forward: nothing returns to `KnownNotSent`, and
    /// a terminal state never changes. Without this, a crash-recovery path
    /// could quietly "reset" an `Uncertain` attempt into a retryable one.
    pub const fn permits_transition_to(self, next: Self) -> bool {
        match (self, next) {
            (Self::KnownNotSent, Self::Sending) => true,
            (Self::Sending, Self::Sent | Self::Uncertain) => true,
            (Self::Sent, Self::Responding | Self::Settled) => true,
            (Self::Uncertain, Self::Responding | Self::Settled) => true,
            (Self::Responding, Self::Settled) => true,
            // A prepared request that is abandoned before dispatch is still
            // provably unsent, so this is the one non-advancing legal case.
            (Self::KnownNotSent, Self::KnownNotSent) => true,
            _ => false,
        }
    }
}

/// One durable provider attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderAttempt {
    /// Stable contract identifier, always [`GROK_ATTEMPT_CONTRACT_VERSION`].
    pub contract: String,
    /// Numeric schema revision, always [`GROK_ATTEMPT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// This attempt's own identity.
    pub attempt_id: BoundedId,
    /// The durable run this attempt belongs to.
    pub run_id: BoundedId,
    /// One-based ordinal within that run, so attempts order without a clock.
    pub ordinal: u32,
    /// Who and where this attempt acts for.
    pub subject: AttemptSubject,
    /// Every authority revision it was decided under.
    pub authority: AuthorityRevisions,
    /// The exact route it is bound to.
    pub route: AttemptRoute,
    /// The immutable statement of what it is for.
    pub intent: AttemptIntent,
    /// Whether the request reached the provider.
    pub send_state: SendState,
    /// Opaque provider-assigned receipts, once there are any.
    #[serde(default)]
    pub receipts: ProviderReceipts,
    /// The typed failure, when this attempt ended in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunFailureKind>,
}

impl ProviderAttempt {
    /// Open a new attempt in the only state that is safe to retry.
    #[allow(clippy::too_many_arguments)] // Every binding is deliberate and explicit.
    pub fn open(
        attempt_id: BoundedId,
        run_id: BoundedId,
        ordinal: u32,
        subject: AttemptSubject,
        authority: AuthorityRevisions,
        route: AttemptRoute,
        intent: AttemptIntent,
    ) -> Self {
        Self {
            contract: GROK_ATTEMPT_CONTRACT_VERSION.to_string(),
            schema_version: GROK_ATTEMPT_SCHEMA_VERSION,
            attempt_id,
            run_id,
            ordinal,
            subject,
            authority,
            route,
            intent,
            send_state: SendState::KnownNotSent,
            receipts: ProviderReceipts::default(),
            failure: None,
        }
    }

    /// Whether the host may re-send this attempt without asking anyone.
    pub const fn may_auto_retry(&self) -> bool {
        self.send_state.may_auto_retry()
    }

    /// Advance the send state, refusing any transition the lattice forbids.
    ///
    /// Returns the state that was rejected, so a caller can record *what* it
    /// tried rather than only that it failed.
    pub fn advance(&mut self, next: SendState) -> Result<(), &'static str> {
        if !self.send_state.permits_transition_to(next) {
            return Err("provider attempt send state cannot move backwards or skip a step");
        }
        self.send_state = next;
        Ok(())
    }

    /// Record a typed failure without ever claiming the attempt succeeded.
    pub fn fail(&mut self, kind: RunFailureKind) -> TerminalVerdict {
        self.failure = Some(kind);
        kind.verdict()
    }

    /// Whether a *new* attempt may be opened for the same intent.
    ///
    /// Only a provably unsent request or a fully settled one may be followed
    /// by an equivalent send. `Sent` and `Responding` have already left the
    /// host; opening another attempt beside them is a duplicate charge even
    /// when the operator does not yet need to reconcile an `Uncertain` gap.
    pub const fn permits_equivalent_retry(&self) -> bool {
        matches!(
            self.send_state,
            SendState::KnownNotSent | SendState::Settled
        )
    }

    /// Validate a durable attempt record before writing or publishing it.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.contract != GROK_ATTEMPT_CONTRACT_VERSION {
            return Err("attempt contract identifier does not match this revision");
        }
        if self.schema_version != GROK_ATTEMPT_SCHEMA_VERSION {
            return Err("attempt schema version does not match this revision");
        }
        if self.ordinal == 0 {
            return Err("attempt ordinal is one-based");
        }
        if !self.attempt_id.is_bounded() || !self.run_id.is_bounded() {
            return Err("attempt identity is not a bounded opaque identifier");
        }
        self.subject.validate()?;
        self.route.validate()?;
        self.intent.validate()?;
        self.receipts.validate()?;
        // A provider receipt is proof the request arrived, so it cannot
        // coexist with a claim that it never left.
        if self.receipts.acknowledged()
            && matches!(
                self.send_state,
                SendState::KnownNotSent | SendState::Sending
            )
        {
            return Err("an attempt with provider receipts cannot claim it was not sent");
        }
        // `Sent` means the provider acknowledged it; without a receipt the
        // honest state is `Uncertain`.
        if self.send_state == SendState::Sent && !self.receipts.acknowledged() {
            return Err("a sent attempt must carry at least one provider receipt");
        }
        // A failure that was caught before dispatch must not claim otherwise.
        if let Some(failure) = self.failure
            && failure.class() == crate::outcome::RunOutcomeClass::Blocked
            && self.send_state != SendState::KnownNotSent
        {
            return Err("a blocked attempt never reached the provider and cannot be terminal");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountReferenceSource;
    use crate::outcome::RunOutcomeClass;

    fn id(value: &str) -> BoundedId {
        BoundedId::new(value).unwrap_or_else(|| panic!("{value:?} should be bounded"))
    }

    fn subject() -> AttemptSubject {
        AttemptSubject {
            principal: Some(id("prn-0a1b2c3d")),
            tenant: Some(id("tnt-9z8y")),
            project: Some(id("prj-alpha")),
            workspace: id("wsp-7f6e"),
            session: id("ses-1122"),
        }
    }

    fn authority() -> AuthorityRevisions {
        AuthorityRevisions {
            auth: Revision(7),
            policy: Revision(3),
            capability: Revision(11),
            credential: Revision(2),
        }
    }

    fn route() -> AttemptRoute {
        AttemptRoute {
            provider: ProviderClass::Xai,
            profile: Some(id("xai")),
            credential_method: CredentialMethod::GrokBuildOidc,
            route: RouteClass::XaiFirstParty,
            base: BaseCategory::XaiOfficial,
            dialect: RequestDialect::XaiChatCompletions,
            model: ModelReference::new("grok-4").expect("bounded model"),
            effort: Some(id("high")),
            account_reference: AccountReference::new(
                "usr-0a1b2c3d",
                AccountReferenceSource::UserId,
            ),
        }
    }

    fn intent() -> AttemptIntent {
        AttemptIntent {
            digest: id("sha256:0a1b2c3d4e5f"),
            request_id: id("req-0001"),
            provider_idempotency_key: id("idem-0a1b2c3d"),
        }
    }

    fn attempt() -> ProviderAttempt {
        ProviderAttempt::open(
            id("att-0001"),
            id("run-0001"),
            1,
            subject(),
            authority(),
            route(),
            intent(),
        )
    }

    fn acknowledged() -> ProviderReceipts {
        ProviderReceipts {
            request: Some(id("prq-abc123")),
            run: Some(id("prn-def456")),
            usage: Some(UsageReceipt {
                input_tokens: 1_200,
                output_tokens: 340,
            }),
            provider_replied: true,
        }
    }

    #[test]
    fn a_new_attempt_is_the_only_state_that_may_be_retried_by_itself() {
        let attempt = attempt();
        assert_eq!(attempt.send_state, SendState::KnownNotSent);
        assert!(attempt.may_auto_retry());
        assert!(attempt.permits_equivalent_retry());
        assert_eq!(attempt.validate(), Ok(()));
    }

    /// The central safety property. A retry that duplicates delivered work is
    /// a second charge and a second set of side effects.
    #[test]
    fn only_a_provably_unsent_attempt_may_auto_retry() {
        for state in SendState::ALL {
            assert_eq!(
                state.may_auto_retry(),
                state == SendState::KnownNotSent,
                "{state:?} disagreed about auto-retry"
            );
        }
        // Specifically: an interrupted send is not auto-retryable, however
        // much it looks like one that never left.
        assert!(!SendState::Sending.may_auto_retry());
        assert!(!SendState::Uncertain.may_auto_retry());
    }

    #[test]
    fn the_send_lattice_never_moves_backwards_or_skips_a_step() {
        use SendState::*;
        let legal = [
            (KnownNotSent, KnownNotSent),
            (KnownNotSent, Sending),
            (Sending, Sent),
            (Sending, Uncertain),
            (Sent, Responding),
            (Sent, Settled),
            (Uncertain, Responding),
            (Uncertain, Settled),
            (Responding, Settled),
        ];
        for from in SendState::ALL {
            for to in SendState::ALL {
                let permitted = legal.contains(&(from, to));
                assert_eq!(
                    from.permits_transition_to(to),
                    permitted,
                    "{from:?} -> {to:?} disagreed"
                );
            }
        }
        // A settled attempt is finished; nothing reopens it.
        for to in SendState::ALL {
            assert!(
                !Settled.permits_transition_to(to),
                "Settled -> {to:?} reopened a finished attempt"
            );
        }
        // And nothing ever returns to the retryable state.
        for from in [Sending, Sent, Uncertain, Responding, Settled] {
            assert!(
                !from.permits_transition_to(KnownNotSent),
                "{from:?} was rewound into an auto-retryable state"
            );
        }
    }

    /// A process that dies between dispatch and reply leaves `Sending` on
    /// disk. Recovery must not read that as "never sent".
    #[test]
    fn a_crash_between_dispatch_and_reply_is_not_auto_retryable() {
        let mut attempt = attempt();
        attempt
            .advance(SendState::Sending)
            .expect("dispatch begins");
        // Simulate the crash: the record on disk is exactly this.
        let recovered: ProviderAttempt =
            serde_json::from_value(serde_json::to_value(&attempt).unwrap()).unwrap();
        assert_eq!(recovered.send_state, SendState::Sending);
        assert!(
            !recovered.may_auto_retry(),
            "an interrupted send was auto-retried"
        );
        assert!(
            !recovered.permits_equivalent_retry(),
            "an equivalent request was allowed while the first is unreconciled"
        );
        assert!(recovered.send_state.requires_reconciliation());
        assert_eq!(recovered.validate(), Ok(()));

        // Recovery cannot rewind it into a retryable state.
        let mut recovered = recovered;
        assert!(recovered.advance(SendState::KnownNotSent).is_err());
        assert_eq!(recovered.send_state, SendState::Sending);
    }

    /// A reply that cannot be parsed is not a failure to send: the request may
    /// well have run, so it is `Uncertain`, not retryable.
    #[test]
    fn an_unparseable_reply_leaves_the_attempt_uncertain_and_unreconciled() {
        let mut attempt = attempt();
        attempt.advance(SendState::Sending).unwrap();
        attempt.advance(SendState::Uncertain).unwrap();
        let verdict = attempt.fail(RunFailureKind::MalformedOutput);
        assert_eq!(verdict.class, RunOutcomeClass::Indeterminate);
        assert!(!verdict.claims_success());
        assert!(!attempt.may_auto_retry());
        assert!(!attempt.permits_equivalent_retry());
        assert_eq!(attempt.validate(), Ok(()));
    }

    /// A refusal caught before dispatch provably spent nothing, so a fresh
    /// equivalent request is safe.
    #[test]
    fn a_pre_dispatch_refusal_stays_retryable() {
        let mut attempt = attempt();
        let verdict = attempt.fail(RunFailureKind::CredentialExpired);
        assert_eq!(verdict.class, RunOutcomeClass::Blocked);
        assert!(!verdict.claims_success());
        assert_eq!(attempt.send_state, SendState::KnownNotSent);
        assert!(attempt.may_auto_retry());
        assert_eq!(attempt.validate(), Ok(()));
    }

    #[test]
    fn a_blocked_failure_cannot_claim_the_request_reached_the_provider() {
        let mut attempt = attempt();
        attempt.advance(SendState::Sending).unwrap();
        attempt.receipts = acknowledged();
        attempt.advance(SendState::Sent).unwrap();
        attempt.failure = Some(RunFailureKind::CredentialMissing);
        assert!(
            attempt.validate().is_err(),
            "a pre-dispatch block was recorded on a delivered attempt"
        );
    }

    #[test]
    fn receipts_and_send_state_must_agree() {
        // Receipts without delivery.
        let mut smuggled = attempt();
        smuggled.receipts = acknowledged();
        assert!(
            smuggled.validate().is_err(),
            "receipts on an unsent attempt"
        );

        let mut mid_flight = attempt();
        mid_flight.advance(SendState::Sending).unwrap();
        mid_flight.receipts = acknowledged();
        assert!(
            mid_flight.validate().is_err(),
            "receipts while still sending"
        );

        // Delivery without receipts is not `Sent`, it is `Uncertain`.
        let mut unwitnessed = attempt();
        unwitnessed.advance(SendState::Sending).unwrap();
        unwitnessed.send_state = SendState::Sent;
        assert!(
            unwitnessed.validate().is_err(),
            "a sent attempt with no provider receipt validated"
        );

        let mut honest = attempt();
        honest.advance(SendState::Sending).unwrap();
        honest.receipts = acknowledged();
        honest.advance(SendState::Sent).unwrap();
        assert_eq!(honest.validate(), Ok(()));
        assert!(!honest.may_auto_retry());
        assert!(
            !honest.permits_equivalent_retry(),
            "a delivered but unsettled attempt must not be followed by a duplicate"
        );
        honest.advance(SendState::Settled).unwrap();
        assert!(
            honest.permits_equivalent_retry(),
            "only a settled attempt stops blocking a later intent"
        );
    }

    #[test]
    fn usage_is_a_count_and_never_an_entitlement_claim() {
        let usage = UsageReceipt {
            input_tokens: 10,
            output_tokens: 20,
        };
        let encoded = serde_json::to_value(usage).expect("usage serializes");
        let keys: Vec<&str> = encoded
            .as_object()
            .expect("usage is an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, vec!["inputTokens", "outputTokens"]);
        // No balance, quota, entitlement, cost, or expiry claim anywhere.
        let published = serde_json::to_string(&attempt()).expect("attempt serializes");
        for forbidden in [
            "balance",
            "quota",
            "entitlement",
            "billing",
            "cost",
            "price",
            "tokenReady",
            "token_ready",
            "expiresAt",
        ] {
            assert!(
                !published.contains(forbidden),
                "the attempt record claims {forbidden:?}"
            );
        }
    }

    #[test]
    fn the_record_never_carries_credential_endpoint_or_prompt_material() {
        let mut sent = attempt();
        sent.advance(SendState::Sending).unwrap();
        sent.receipts = acknowledged();
        sent.advance(SendState::Sent).unwrap();
        let encoded = serde_json::to_string(&sent).expect("attempt serializes");
        for needle in [
            "Bearer",
            "bearer",
            "refresh_token",
            "refreshToken",
            "apiKey",
            "api_key",
            "keychain:",
            "https://",
            "http://",
            "@",
            "/Users/",
            "/home/",
            "prompt",
            "message",
        ] {
            assert!(
                !encoded.contains(needle),
                "attempt leaked {needle:?}: {encoded}"
            );
        }
    }

    #[test]
    fn bounded_identifiers_reject_anything_not_recordable_verbatim() {
        for hostile in [
            "",
            "   ",
            "has space",
            "has\nnewline",
            "has\u{0}null",
            "<script>",
            "../../etc/passwd",
            "/leading",
            "trailing/",
            ".dotfile",
            "a..b",
            "semi;colon",
        ] {
            assert_eq!(BoundedId::new(hostile), None, "accepted {hostile:?}");
        }
        assert!(BoundedId::new(&"a".repeat(MAX_ATTEMPT_IDENTIFIER_BYTES)).is_some());
        assert_eq!(
            BoundedId::new(&"a".repeat(MAX_ATTEMPT_IDENTIFIER_BYTES + 1)),
            None
        );
        assert_eq!(BoundedId::new("  run-1  ").unwrap().as_str(), "run-1");
        assert!(BoundedId::new("sha256:0a1b").unwrap().is_bounded());
    }

    /// A decoded record can contain anything the file held, so validation
    /// re-checks bounds rather than trusting the type.
    #[test]
    fn a_doctored_record_is_refused_by_its_own_validator() {
        let base = serde_json::to_value(attempt()).unwrap();

        let mut unbounded_run = base.clone();
        unbounded_run["runId"] = serde_json::json!("../../etc/passwd");
        let decoded: ProviderAttempt = serde_json::from_value(unbounded_run).unwrap();
        assert!(
            decoded.validate().is_err(),
            "a traversal-shaped run id validated"
        );

        let mut zero_ordinal = base.clone();
        zero_ordinal["ordinal"] = serde_json::json!(0);
        let decoded: ProviderAttempt = serde_json::from_value(zero_ordinal).unwrap();
        assert!(decoded.validate().is_err(), "a zero ordinal validated");

        let mut wrong_contract = base.clone();
        wrong_contract["contract"] = serde_json::json!("grokptah.attempt.v2");
        let decoded: ProviderAttempt = serde_json::from_value(wrong_contract).unwrap();
        assert!(decoded.validate().is_err());

        let mut unbounded_model = base.clone();
        unbounded_model["route"]["model"]["value"] = serde_json::json!("grok-4 <script>");
        let decoded: ProviderAttempt = serde_json::from_value(unbounded_model).unwrap();
        assert!(decoded.validate().is_err(), "an unbounded model validated");

        let mut extra = base;
        extra["balanceUsd"] = serde_json::json!(42);
        assert!(
            serde_json::from_value::<ProviderAttempt>(extra).is_err(),
            "deny_unknown_fields let an extra claim through"
        );
    }

    #[test]
    fn every_authority_revision_is_recorded_so_drift_is_detectable_after_the_fact() {
        let attempt = attempt();
        let encoded = serde_json::to_value(&attempt).unwrap();
        let authority = encoded["authority"].as_object().expect("authority object");
        let mut keys: Vec<&str> = authority.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["auth", "capability", "credential", "policy"]);

        // A superseded decision is detectable by comparison alone.
        let mut later = attempt.clone();
        later.authority.policy = Revision(4);
        assert_ne!(later.authority, attempt.authority);
        assert!(later.authority.policy > attempt.authority.policy);
    }

    #[test]
    fn the_full_binding_round_trips_and_pins_its_wire_shape() {
        let attempt = attempt();
        let encoded = serde_json::to_value(&attempt).expect("attempt serializes");
        let decoded: ProviderAttempt =
            serde_json::from_value(encoded.clone()).expect("attempt round-trips");
        assert_eq!(decoded, attempt);
        let mut keys: Vec<&str> = encoded
            .as_object()
            .expect("attempt is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "attemptId",
                "authority",
                "contract",
                "intent",
                "ordinal",
                "receipts",
                "route",
                "runId",
                "schemaVersion",
                "sendState",
                "subject",
            ]
        );
        for state in SendState::ALL {
            assert_eq!(
                serde_json::to_string(&state).expect("state serializes"),
                format!("\"{}\"", state.as_str())
            );
        }
    }
}
