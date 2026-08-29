//! Binding a durable run to the provider attempts it issues.
//!
//! [`crate::launch_truth`] decides whether a run may start.  This module
//! records what each individual provider request was bound to, and — the part
//! that is expensive to get wrong — whether that request actually left this
//! host.
//!
//! The rule the whole module exists to enforce: a request that is `sending` or
//! `uncertain` is *not* safe to repeat. Cancel and restart must reconcile the
//! exact recorded attempt against the provider's idempotency key rather than
//! opening a fresh equivalent request beside it.

use grokptah_agent_sdk::attempt::{
    AttemptIntent, AttemptRoute, AttemptSubject, AuthorityRevisions, BoundedId, ProviderAttempt,
    ProviderReceipts, Revision, SendState, UsageReceipt,
};
use grokptah_agent_sdk::launch::LaunchRequirement;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::launch_truth::AdmissionFacts;
use crate::orchestration::OrchStore;
use crate::physical_send::PhysicalSendBinding;

/// Refusal text for a run whose earlier attempt is still unreconciled.
///
/// One string, shared by every caller, because it is the sentence an operator
/// reads when a send is refused: it must name the action (reconcile against
/// the recorded key) rather than describing an internal state.
pub(crate) const UNRECONCILED_REFUSAL: &str =
    "an earlier provider attempt for this run is still unreconciled; reconcile it against its \
     idempotency key before issuing an equivalent request";

/// Everything the host knows about *who* a run acts for, before binding.
///
/// Carries no path, display name, or email: the workspace arrives already
/// reduced to an opaque handle by [`workspace_handle`].
#[derive(Debug, Clone)]
pub(crate) struct RunPrincipalContext {
    /// Opaque durable identity of the acting tenant, when there is one.
    pub tenant: Option<String>,
    /// Opaque durable identity of the project, when there is one.
    pub project: Option<String>,
    /// Approved workspace, already reduced to an opaque handle.
    pub workspace: String,
    /// Owning session identity.
    pub session: Uuid,
    /// Revisions of the decisions this run was admitted under.
    pub authority: AuthorityRevisions,
}

/// Reduce a workspace path to a bounded opaque handle.
///
/// A workspace path is host detail — it can contain a user name, a customer
/// name, or a project code — so the durable attempt records a digest of it
/// rather than the path. The digest is stable, so two attempts against the
/// same workspace still compare equal.
pub fn workspace_handle(workspace: &str) -> BoundedId {
    digest_handle("wsp", workspace)
}

/// Reduce arbitrary text to a bounded opaque handle.
fn digest_handle(prefix: &str, value: &str) -> BoundedId {
    let digest = Sha256::digest(value.as_bytes());
    let hex: String = digest
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    BoundedId::new(&format!("{prefix}:{hex}"))
        .expect("a hex digest with a fixed prefix is always bounded")
}

/// The opaque intent digest for one exact request.
///
/// Digests the prompt rather than recording it, so a retry can be recognised
/// as the same intent without any durable record holding user text.
pub fn intent_digest(prompt: &str) -> BoundedId {
    digest_handle("sha256", prompt)
}

/// The idempotency key presented to the provider for one attempt.
///
/// Derived from the run and the ordinal rather than randomly, so a host that
/// crashes and re-reads its own record produces the identical key and the
/// provider can recognise the duplicate.
pub fn provider_idempotency_key(run_id: &str, ordinal: u32) -> BoundedId {
    digest_handle("idem", &format!("{run_id}#{ordinal}"))
}

/// Bind one provider attempt to everything that decided it.
///
/// Returns `None` when the admission carried no enforced facts — an offline
/// host reaches no provider, so there is no attempt to record and recording
/// one would imply a request that can never exist.
pub(crate) fn bind_attempt(
    run_id: &str,
    ordinal: u32,
    request_id: &str,
    prompt: &str,
    context: &RunPrincipalContext,
    admission: Option<&AdmissionFacts>,
) -> Option<ProviderAttempt> {
    let facts = admission?;
    let requirement = &facts.requirement;
    let subject = AttemptSubject {
        // A bare API-key route publishes no durable principal; recording that
        // honestly is the binding, not a gap in it.
        principal: requirement
            .account_reference
            .as_ref()
            .and_then(|reference| BoundedId::new(&reference.value)),
        tenant: context.tenant.as_deref().and_then(BoundedId::new),
        project: context.project.as_deref().and_then(BoundedId::new),
        workspace: workspace_handle(&context.workspace),
        session: digest_handle("ses", &context.session.to_string()),
    };
    let route = attempt_route(requirement);
    let intent = AttemptIntent {
        digest: intent_digest(prompt),
        request_id: BoundedId::new(request_id).unwrap_or_else(|| digest_handle("req", request_id)),
        provider_idempotency_key: provider_idempotency_key(run_id, ordinal),
    };
    Some(ProviderAttempt::open(
        digest_handle("att", &format!("{run_id}#{ordinal}")),
        BoundedId::new(run_id).unwrap_or_else(|| digest_handle("run", run_id)),
        ordinal,
        subject,
        context.authority,
        route,
        intent,
    ))
}

/// Project the pinned launch requirement onto the attempt's route binding.
///
/// The same closed vocabularies are reused deliberately: a drift between what
/// a run was admitted on and what an attempt was sent under is then a
/// type-level comparison rather than a string match.
fn attempt_route(requirement: &LaunchRequirement) -> AttemptRoute {
    AttemptRoute {
        provider: requirement.provider,
        profile: None,
        credential_method: requirement.credential_method,
        route: requirement.route,
        base: requirement.base,
        dialect: requirement.dialect,
        model: requirement
            .model
            .clone()
            // A ready requirement always carries a bounded model; this keeps
            // the binding total without inventing a plausible-looking id.
            .unwrap_or_else(|| {
                grokptah_agent_sdk::launch::ModelReference::new("unresolved")
                    .expect("a literal placeholder is bounded")
            }),
        effort: None,
        account_reference: requirement.account_reference.clone(),
    }
}

/// Attach the selected provider profile and reasoning effort to a binding.
pub(crate) fn with_selection(
    mut attempt: ProviderAttempt,
    profile_id: Option<&str>,
    effort: Option<&str>,
) -> ProviderAttempt {
    attempt.route.profile = profile_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(BoundedId::new);
    attempt.route.effort = effort
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(BoundedId::new);
    attempt
}

/// Whether a run may issue a new equivalent provider request.
///
/// The durable form of the auto-retry rule: false while any recorded attempt
/// still needs provider-side reconciliation.
pub(crate) fn permits_new_request(attempts: &[ProviderAttempt]) -> bool {
    attempts
        .iter()
        .all(ProviderAttempt::permits_equivalent_retry)
}

/// The next ordinal for a run, one-based and gap-free.
pub(crate) fn next_ordinal(attempts: &[ProviderAttempt]) -> u32 {
    attempts
        .iter()
        .map(|attempt| attempt.ordinal)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
}

/// Reconcile an attempt that was interrupted rather than answered.
///
/// A cancelled or crashed in-flight request is exactly ambiguous: it may have
/// executed. Moving it to `uncertain` records that ambiguity instead of
/// silently making it look retryable.
pub fn reconcile_interrupted(attempt: &mut ProviderAttempt) -> Result<(), &'static str> {
    match attempt.send_state {
        SendState::Sending => attempt.advance(SendState::Uncertain),
        // A request that never left is still provably unsent. Sent/responding/
        // settled/uncertain records are not rewound.
        SendState::KnownNotSent
        | SendState::Sent
        | SendState::Uncertain
        | SendState::Responding
        | SendState::Settled => Ok(()),
    }
}

/// Convert host-reported token counts into a bounded usage receipt.
pub(crate) fn usage_receipt(input_tokens: u64, output_tokens: u64) -> Option<UsageReceipt> {
    (input_tokens > 0 || output_tokens > 0).then_some(UsageReceipt {
        input_tokens,
        output_tokens,
    })
}

/// The authority revisions a host records when it has no versioned decision
/// store yet.
///
/// Zero is deliberately not "unknown dressed up as a number": it is the
/// initial revision of a decision that has never been superseded, and any
/// later decision compares strictly greater.
pub(crate) const fn initial_authority() -> AuthorityRevisions {
    AuthorityRevisions {
        auth: Revision(0),
        policy: Revision(0),
        capability: Revision(0),
        credential: Revision(0),
    }
}

/// Fold a completed turn's evidence into an attempt's receipts.
///
/// Merges rather than replaces. A request or run identifier published by the
/// provider is the strongest evidence the record holds, and it is written
/// while the response is still on the wire; overwriting it at turn end with a
/// receipt derived from local counters would discard the only proof of *which*
/// provider-side request this attempt became.
pub(crate) fn record_reply(receipts: &mut ProviderReceipts, usage: Option<UsageReceipt>) {
    receipts.provider_replied = true;
    if usage.is_some() {
        receipts.usage = usage;
    }
}

/// Admit a run to attempt a physical send.
///
/// Refuses while any recorded attempt still needs provider-side
/// reconciliation. This is the *decision*, not the crossing: the boundary
/// itself is crossed by the transport, at the instant it has a request to put
/// on a socket (see [`crate::physical_send::mark_sending`]).
///
/// Separating them is what keeps the record truthful. A turn is not
/// necessarily a send — a slash command, a session with no resolvable
/// credential, and an offline stub all complete without reaching a provider —
/// so marking `sending` when the turn *starts* would record a request that
/// never existed, and the lattice cannot rewind that.
///
/// The desktop turn runner and the orchestration spawn path share this
/// deliberately: a second implementation would be a second answer to "was this
/// sent?", and the two would drift exactly where it is most expensive.
pub fn admit_send(store: &OrchStore, run_id: &str) -> Result<(), String> {
    let attempts = store
        .list_attempts_for_run(run_id)
        .map_err(|error| error.to_string())?;
    if permits_new_request(&attempts) {
        Ok(())
    } else {
        Err(UNRECONCILED_REFUSAL.into())
    }
}

/// The physical-send binding for this run's prepared attempt.
///
/// Selects the attempt that has provably not been sent, because that is the
/// one the transport is about to send and the one it must move forward.
/// `None` when there is no prepared attempt, which is also the honest binding
/// for an offline host: nothing to carry a key for, nothing to advance.
pub fn send_binding(store: &OrchStore, run_id: &str) -> Option<PhysicalSendBinding> {
    store
        .list_attempts_for_run(run_id)
        .ok()
        .and_then(|attempts| {
            attempts.into_iter().rev().find_map(|attempt| {
                (attempt.send_state == SendState::KnownNotSent).then(|| {
                    PhysicalSendBinding::new(
                        store.clone(),
                        attempt.attempt_id.as_str().to_string(),
                        attempt.intent.provider_idempotency_key.as_str().to_string(),
                        attempt.route.dialect,
                    )
                })
            })
        })
}

/// Move any in-flight attempt for this run to `uncertain`.
///
/// A cancelled or abandoned in-flight request is exactly ambiguous: it may
/// have executed. Recording that is what stops a later restart from quietly
/// duplicating it.
pub(crate) fn reconcile_run(store: &OrchStore, run_id: &str) {
    let Ok(attempts) = store.list_attempts_for_run(run_id) else {
        return;
    };
    for attempt in attempts {
        if attempt.send_state != SendState::Sending {
            continue;
        }
        let _ = store.update_attempt(attempt.attempt_id.as_str(), |attempt| {
            reconcile_interrupted(attempt).map_err(anyhow::Error::msg)
        });
    }
}

/// Settle this run's attempts once the turn is over.
///
/// Deliberately takes no turn outcome. A turn's verdict is not evidence about
/// any individual physical request: a Chat turn renders a failed model call as
/// its reply and returns success, and a tool loop can succeed on a later round
/// while an earlier request remains one nobody ever reported on. Every
/// transition here is therefore a function of what the transport actually
/// recorded, plus the fact that the turn is over.
///
/// So `sent` is never reached from here — only
/// [`crate::physical_send::mark_sent`] can record an acknowledgement — and
/// `uncertain` is absorbing, clearable only by an explicit operator
/// reconciliation. What is left in `sending` at turn end is a request that
/// crossed the boundary and was never reported on, which is exactly
/// `uncertain`.
pub fn settle_run(store: &OrchStore, run_id: &str, usage: Option<UsageReceipt>) {
    let Ok(attempts) = store.list_attempts_for_run(run_id) else {
        return;
    };
    for attempt in attempts {
        let id = attempt.attempt_id.as_str().to_string();
        let next = match attempt.send_state {
            // Reported on by the transport, so the turn's end is its end.
            SendState::Sent | SendState::Responding => SendState::Settled,
            // Crossed the boundary, never reported on again.
            SendState::Sending => SendState::Uncertain,
            // Provably unsent, already ambiguous, or already terminal.
            //
            // `uncertain` is deliberately absorbing here. A turn can succeed on
            // a later round while an earlier physical request remains one
            // nobody ever reported on, so turn success is not evidence about
            // *that* request; settling on it would fabricate the very
            // acknowledgement this module exists to avoid. Only an explicit
            // operator reconciliation may move it.
            SendState::KnownNotSent | SendState::Uncertain | SendState::Settled => continue,
        };
        let _ = store.update_attempt(&id, |attempt| {
            // Receipts are only justified where the record already implies the
            // provider was reached.
            if next == SendState::Settled {
                record_reply(&mut attempt.receipts, usage);
            }
            attempt.advance(next).map_err(anyhow::Error::msg)
        });
    }
}
