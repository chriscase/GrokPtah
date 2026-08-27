//! The one boundary a turn crosses from "decided" to "may have reached a
//! provider".
//!
//! [`crate::launch_truth`] answers *may* a run start and [`crate::attempt_binding`]
//! describes what a request is bound to. This module owns the part that is
//! expensive to get wrong at runtime: making the durable record cross the send
//! boundary **before** the bytes do, and refusing to move it forward on
//! anything weaker than an observed provider receipt.
//!
//! # Why this is a chokepoint rather than a policy
//!
//! A rule that lives beside the send is a rule some other caller forgets. The
//! ledger used to be driven only by the orchestration service, so a run
//! started over MCP was recorded and the identical provider request issued by
//! a desktop Chat turn was not — same credential, same charge, no record.
//! Here the declaration is performed by the code that owns the socket, so a
//! path that reaches a provider without a durable attempt does not typecheck:
//! there is nothing to hand the transport until [`SendLedger::declare`] has
//! returned.
//!
//! # One physical send, one attempt
//!
//! A single logical model step can issue several *physical* requests — a
//! credential refresh after 401, a non-stream fallback, a retry the transport
//! proved never left. Each gets its own ordinal, its own idempotency key, and
//! its own body digest, because each is separately capable of costing money.
//! Ordinals are never reused: a reused key is indistinguishable, to the
//! provider, from the duplicate it exists to suppress.
//!
//! # What may advance the record
//!
//! Only things the provider did:
//!
//! ```text
//! declare ──► KnownNotSent ──► Sending ──► Sent ──► Responding ──► Settled
//!                   │             │          │           │
//!                   │             └──────────┴───────────┴──► Uncertain
//!                   └──► Settled(not_sent)                       │
//!                                                     (reconcile)┘
//! ```
//!
//! `Sending` is written before the socket call and never after it. `Sent`
//! requires a response head, `Responding` requires a status, and `Settled`
//! requires an outcome the provider actually produced. A turn that merely
//! returned a `String` proves none of these — a host that treats its own
//! success as the provider's is exactly how a failed send gets reported as a
//! delivered one.
//!
//! # Fencing, not retrying
//!
//! Every post-boundary failure — timeout, dropped connection, truncated
//! stream, cancellation, a process that died — leaves [`SendState::Uncertain`]
//! and *fences*. Nothing here ever re-sends on its own. The exit from
//! `Uncertain` is an explicit reconciliation against the recorded idempotency
//! key, and no number of restarts or reopens clears it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Result};
use grokptah_agent_sdk::attempt::{
    BoundedId, ProviderAttempt, ProviderReceipts, SendOutcome, SendState, UsageReceipt,
};
use uuid::Uuid;

use crate::attempt_binding::{self, RunPrincipalContext};
use crate::orchestration::OrchStore;
use grokptah_agent_sdk::launch::LaunchRequirement;

/// Hard ceiling on physical sends recorded for one run.
///
/// A send machine that blows through this is refused at the durable boundary
/// rather than being allowed to grow the ledger without bound.
pub const MAX_SENDS_PER_RUN: u32 = 512;

/// Why this physical send exists.
///
/// Every value other than [`SendCause::InitialSend`] is a resend the host
/// decided to issue, and each carries a fresh ordinal and a fresh body digest
/// so the ledger never conflates two different requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendCause {
    /// First physical send for this model step.
    InitialSend,
    /// HTTP 401 answered with a refreshed credential.
    AuthRefresh,
    /// A connect-phase failure that proved the previous request never left.
    TransportRetry,
    /// HTTP 429; the provider answered and declined to do the work.
    RateLimitRetry,
    /// HTTP 5xx or 408; the provider answered with a non-success status.
    ServerErrorRetry,
    /// The gateway rejected the request shape; resend without the field.
    RequestShapeFallback,
    /// The gateway rejected or emptied the streaming contract; resend
    /// non-stream.
    StreamFallback,
}

impl SendCause {
    /// The exact value recorded in the durable record.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InitialSend => "initial_send",
            Self::AuthRefresh => "auth_refresh",
            Self::TransportRetry => "transport_retry",
            Self::RateLimitRetry => "rate_limit_retry",
            Self::ServerErrorRetry => "server_error_retry",
            Self::RequestShapeFallback => "request_shape_fallback",
            Self::StreamFallback => "stream_fallback",
        }
    }

    /// Whether this cause may follow an attempt that is not settled.
    ///
    /// Only [`SendCause::TransportRetry`] may, and only because a connect
    /// failure is positive evidence the previous request never reached the
    /// transport. Every other resend follows an attempt the provider saw, so
    /// it must settle first.
    const fn follows_unresolved(self) -> bool {
        matches!(self, Self::TransportRetry)
    }
}

/// The exact request one physical send is bound to.
///
/// Every field is a digest or a closed value. No URL, hostname, bearer,
/// prompt, or response body reaches a durable record through here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRequestIdentity {
    /// Opaque digest of the endpoint the request is addressed to.
    pub route_digest: BoundedId,
    /// Opaque digest of the exact bytes handed to the transport.
    pub body_digest: BoundedId,
    /// Opaque digest of the credential material in use, so a rotation or a
    /// refresh is detectable without the record holding the secret.
    pub credential_revision: BoundedId,
}

/// Everything a turn needs to bind before it may reach a provider.
#[derive(Debug, Clone)]
pub struct SendBinding {
    /// The durable run this send belongs to.
    pub run_id: String,
    /// The caller's idempotency key for the originating intent.
    pub request_id: String,
    /// The owning session.
    pub session_id: Uuid,
    /// The approved workspace, still a path here and reduced to an opaque
    /// handle before anything is written.
    pub workspace: String,
    /// The exact prompt, digested rather than recorded.
    pub prompt: String,
    /// The exact facts the turn was admitted on, when the host reached a
    /// provider at all.
    ///
    /// `None` is an offline host: it issues no request, so there is no attempt
    /// to record and recording one would imply a request that cannot exist.
    pub requirement: Option<LaunchRequirement>,
    /// The selected provider profile, when one was chosen.
    pub profile: Option<String>,
    /// The selected reasoning effort, when one was chosen.
    pub effort: Option<String>,
}

/// The durable ledger a turn declares its physical sends against.
///
/// Cloneable and shareable: the same ledger is handed to every send site
/// inside one turn so their ordinals come from one place.
#[derive(Clone)]
pub struct SendLedger {
    store: OrchStore,
    binding: Arc<SendBinding>,
}

impl SendLedger {
    /// Bind a turn to the ledger it will declare its sends against.
    ///
    /// Returns `None` when the admission reached no provider: an offline host
    /// issues no request, and recording an attempt would imply one that can
    /// never exist.
    pub fn bind(store: OrchStore, binding: SendBinding) -> Option<Self> {
        binding.requirement.as_ref()?;
        Some(Self {
            store,
            binding: Arc::new(binding),
        })
    }

    /// The run this ledger records against.
    pub fn run_id(&self) -> &str {
        &self.binding.run_id
    }

    /// Fence every attempt for this run whose outcome nobody observed.
    ///
    /// Idempotent, and it only touches unresolved records: a turn whose sends
    /// all settled normally passes through here without changing anything.
    pub fn fence(&self) {
        fence_run(&self.store, &self.binding.run_id);
    }

    /// Declare one physical provider send and persist it *before* the socket.
    ///
    /// The returned ticket is the only way to advance the record, so a call
    /// site that never declared has nothing to advance and a call site that
    /// did cannot forget to. A refusal here is a refusal to send.
    pub fn declare(
        &self,
        cause: SendCause,
        identity: &ProviderRequestIdentity,
    ) -> Result<AttemptTicket> {
        let recorded = self
            .store
            .list_attempts_for_run(&self.binding.run_id)
            .unwrap_or_default();

        // The duplicate-send refusal. An unresolved attempt means the provider
        // may still be holding work for an equivalent request, and issuing a
        // second one beside it is the duplicate charge the idempotency key
        // exists to prevent -- the key only helps if the provider is asked
        // about the *recorded* attempt rather than handed a fresh one.
        if !cause.follows_unresolved() {
            let unresolved: Vec<&ProviderAttempt> = recorded
                .iter()
                .filter(|attempt| attempt.is_unresolved())
                .collect();
            if let Some(blocking) = unresolved.first() {
                return Err(anyhow!(
                    "refusing to send: attempt {} for this run is {} and must be reconciled \
                     against idempotency key {} first",
                    blocking.attempt_id.as_str(),
                    blocking.send_state.as_str(),
                    blocking.intent.provider_idempotency_key.as_str()
                ));
            }
        }

        let ordinal = attempt_binding::next_ordinal(&recorded);
        if ordinal > MAX_SENDS_PER_RUN {
            return Err(anyhow!(
                "refusing to send: this run has already recorded {MAX_SENDS_PER_RUN} physical \
                 provider sends"
            ));
        }

        let Some(attempt) = attempt_binding::bind_attempt(
            &self.binding.run_id,
            ordinal,
            &self.binding.request_id,
            &self.binding.prompt,
            &RunPrincipalContext {
                tenant: None,
                project: None,
                workspace: self.binding.workspace.clone(),
                session: self.binding.session_id,
                authority: attempt_binding::initial_authority(),
            },
            self.binding.requirement.as_ref(),
        ) else {
            // `bind` already refused an admission with no enforced facts, so
            // this is unreachable in practice and stays fail-closed rather
            // than silently dispatching unrecorded.
            return Err(anyhow!(
                "refusing to send: this turn has no enforced admission to bind an attempt to"
            ));
        };

        let mut attempt = attempt_binding::with_selection(
            attempt,
            self.binding.profile.as_deref(),
            self.binding.effort.as_deref(),
        );
        attempt.intent.body_digest = Some(identity.body_digest.clone());
        attempt.route.route_digest = Some(identity.route_digest.clone());
        attempt.route.credential_digest = Some(identity.credential_revision.clone());

        self.store
            .open_attempt(&attempt)
            .map_err(|error| anyhow!("could not record the provider attempt: {error}"))?;

        Ok(AttemptTicket {
            store: Some(self.store.clone()),
            attempt_id: attempt.attempt_id.as_str().to_string(),
            idempotency_key: attempt.intent.provider_idempotency_key.as_str().to_string(),
            request_id: attempt.intent.request_id.as_str().to_string(),
            cause,
            resolved: Arc::new(AtomicBool::new(false)),
        })
    }
}

/// One declared physical send, and the only handle that may advance it.
///
/// Dropping an unresolved ticket fences it: an in-flight request whose owner
/// went away is exactly ambiguous, and recording that is what stops a later
/// restart from quietly repeating it.
pub struct AttemptTicket {
    store: Option<OrchStore>,
    attempt_id: String,
    idempotency_key: String,
    request_id: String,
    cause: SendCause,
    resolved: Arc<AtomicBool>,
}

impl std::fmt::Debug for AttemptTicket {
    /// Names the attempt without reproducing anything bound to it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttemptTicket")
            .field("attempt_id", &self.attempt_id)
            .field("cause", &self.cause.as_str())
            .field("bound", &self.store.is_some())
            .field("resolved", &self.resolved.load(Ordering::SeqCst))
            .finish()
    }
}

impl AttemptTicket {
    /// A ticket for a turn with no ledger behind it.
    ///
    /// Used by offline hosts and by call sites with no durable run: every
    /// method is a no-op, so the send path has one shape rather than a
    /// nullable one that is easy to forget to check.
    pub fn unbound() -> Self {
        Self {
            store: None,
            attempt_id: String::new(),
            idempotency_key: String::new(),
            request_id: String::new(),
            cause: SendCause::InitialSend,
            resolved: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Whether this ticket records against a durable ledger at all.
    pub fn is_bound(&self) -> bool {
        self.store.is_some()
    }

    /// The idempotency key this send must present to the provider.
    ///
    /// Presenting the recorded key rather than a fresh one is what makes a
    /// reconciliation possible: without it the provider cannot be asked
    /// whether *this* request already ran.
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// The caller-facing request identifier bound to this send.
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    /// The durable attempt this ticket advances.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }

    /// Why this physical send exists.
    pub fn cause(&self) -> SendCause {
        self.cause
    }

    /// Persist `sending` **before** handing any byte to the transport.
    ///
    /// A process killed after this leaves `sending` on disk, which is
    /// deliberately not auto-retryable: an interrupted send is
    /// indistinguishable from a delivered one without asking the provider.
    pub fn mark_sending(&self) -> Result<()> {
        self.update("mark sending", |attempt| {
            attempt
                .advance(SendState::Sending)
                .map_err(anyhow::Error::msg)
        })
    }

    /// Record that the provider proved receipt by producing a response head.
    pub fn mark_sent(&self, provider_request_id: Option<&str>, status: u16) -> Result<()> {
        let request = provider_request_id.and_then(BoundedId::new);
        self.update("mark sent", move |attempt| {
            attempt.receipts.request = request.clone();
            attempt.receipts.response_status = Some(status);
            attempt.advance(SendState::Sent).map_err(anyhow::Error::msg)
        })
    }

    /// Record that the provider's answer is being consumed.
    pub fn mark_responding(&self) -> Result<()> {
        self.update("mark responding", |attempt| {
            attempt
                .advance(SendState::Responding)
                .map_err(anyhow::Error::msg)
        })
    }

    /// Settle a send the provider completed successfully.
    ///
    /// The usage counts come from the provider's own report; a host estimate
    /// is not a receipt and is not recorded as one.
    pub fn settle_accepted(&self, usage: Option<UsageReceipt>, run: Option<&str>) -> Result<()> {
        let run = run.and_then(BoundedId::new);
        self.resolve("settle accepted", move |attempt| {
            attempt.receipts.provider_replied = true;
            if usage.is_some() {
                attempt.receipts.usage = usage;
            }
            if run.is_some() {
                attempt.receipts.run = run.clone();
            }
            attempt
                .settle(SendOutcome::Accepted)
                .map_err(anyhow::Error::msg)
        })
    }

    /// Settle a send the provider definitively refused.
    ///
    /// A refusal is still a delivery: the provider received the request and
    /// answered it. Recording it as anything else would fence a route that is
    /// working exactly as designed.
    pub fn settle_rejected(&self, status: u16) -> Result<()> {
        self.resolve("settle rejected", move |attempt| {
            attempt.receipts.response_status = Some(status);
            attempt
                .settle(SendOutcome::Rejected)
                .map_err(anyhow::Error::msg)
        })
    }

    /// Settle a send that provably never reached the transport.
    pub fn settle_not_sent(&self) -> Result<()> {
        self.resolve("settle not sent", |attempt| {
            attempt
                .settle(SendOutcome::NotSent)
                .map_err(anyhow::Error::msg)
        })
    }

    /// Fence this send: the outcome is unknown and remote work may be live.
    ///
    /// Deliberately not a failure path that "cleans up". The record stays
    /// unresolved until someone reconciles it, and every equivalent request is
    /// refused until they do.
    pub fn mark_uncertain(&self) -> Result<()> {
        self.resolve("fence as uncertain", |attempt| {
            if attempt.send_state == SendState::Uncertain {
                return Ok(());
            }
            attempt
                .advance(SendState::Uncertain)
                .map_err(anyhow::Error::msg)
        })
    }

    fn update<F>(&self, what: &str, mutate: F) -> Result<()>
    where
        F: Fn(&mut ProviderAttempt) -> Result<()>,
    {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        let updated = store
            .update_attempt(&self.attempt_id, |attempt| mutate(attempt))
            .map_err(|error| {
                anyhow!("could not {what} for attempt {}: {error}", self.attempt_id)
            })?;
        if updated.is_none() {
            return Err(anyhow!(
                "could not {what}: attempt {} is no longer recorded",
                self.attempt_id
            ));
        }
        Ok(())
    }

    fn resolve<F>(&self, what: &str, mutate: F) -> Result<()>
    where
        F: Fn(&mut ProviderAttempt) -> Result<()>,
    {
        let outcome = self.update(what, mutate);
        // Mark resolved even when the write failed: a second attempt from
        // `Drop` would fail identically and only add noise. The record on disk
        // is still unresolved, which is the honest state and the one restart
        // reconciliation reads.
        self.resolved.store(true, Ordering::SeqCst);
        outcome
    }
}

impl Drop for AttemptTicket {
    fn drop(&mut self) {
        if self.store.is_none() || self.resolved.load(Ordering::SeqCst) {
            return;
        }
        // The owner went away mid-send. Fence rather than tidy up: a request
        // whose outcome nobody observed may well have executed.
        let _ = self.update("fence a dropped attempt", |attempt| {
            if attempt.send_state.is_unresolved() && attempt.send_state != SendState::Uncertain {
                attempt
                    .advance(SendState::Uncertain)
                    .map_err(anyhow::Error::msg)
            } else {
                Ok(())
            }
        });
    }
}

/// Every attempt for a run that still needs provider-side reconciliation.
pub fn unresolved_attempts(store: &OrchStore, run_id: &str) -> Vec<ProviderAttempt> {
    store
        .list_attempts_for_run(run_id)
        .unwrap_or_default()
        .into_iter()
        .filter(ProviderAttempt::is_unresolved)
        .collect()
}

/// Fence every in-flight attempt for a run.
///
/// Called on cancellation, timeout, and restart. All three are the same fact:
/// nobody observed how the request ended.
pub fn fence_run(store: &OrchStore, run_id: &str) {
    let Ok(attempts) = store.list_attempts_for_run(run_id) else {
        return;
    };
    for attempt in attempts {
        if !attempt.is_unresolved() || attempt.send_state == SendState::Uncertain {
            continue;
        }
        let _ = store.update_attempt(attempt.attempt_id.as_str(), |attempt| {
            attempt
                .advance(SendState::Uncertain)
                .map_err(anyhow::Error::msg)
        });
    }
}

/// The receipts a settled, accepted send carries.
///
/// Built only from provider-reported counts, so an empty report stays empty
/// rather than being filled in with a host estimate.
pub fn accepted_receipts(
    status: u16,
    request: Option<BoundedId>,
    usage: Option<UsageReceipt>,
) -> ProviderReceipts {
    ProviderReceipts {
        request,
        run: None,
        usage,
        provider_replied: true,
        response_status: Some(status),
        outcome: Some(SendOutcome::Accepted),
    }
}
