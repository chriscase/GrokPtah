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
    Revision, SendState,
};
use grokptah_agent_sdk::launch::LaunchRequirement;
use sha2::{Digest, Sha256};
use uuid::Uuid;

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

/// Reduce an endpoint to a bounded opaque handle.
///
/// A base URL is host detail — a private gateway hostname can name a customer
/// or an internal service — so the durable attempt records a digest of it.
/// Two sends to the same endpoint still compare equal, which is the whole
/// point: it makes a silent re-point detectable without publishing where the
/// request went.
pub fn route_digest(base_url: &str) -> BoundedId {
    digest_handle("route", base_url.trim().trim_end_matches('/'))
}

/// Reduce the exact wire body to a bounded opaque handle.
///
/// Digests the serialized request rather than recording it, so the durable
/// record can prove which bytes were sent without holding the prompt.
pub fn body_digest(body: &serde_json::Value) -> BoundedId {
    digest_handle("body", &serde_json::to_string(body).unwrap_or_default())
}

/// Reduce credential material to a bounded opaque handle.
///
/// The digest covers the identity *and* the bearer, so a refresh that swaps
/// the token without changing the account still moves it. The bearer itself
/// never leaves this function.
pub fn credential_digest(identity: &serde_json::Value, bearer: &str) -> BoundedId {
    let mut hasher = Sha256::new();
    hasher.update(
        serde_json::to_string(identity)
            .unwrap_or_default()
            .as_bytes(),
    );
    hasher.update([0u8]);
    hasher.update(bearer.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    BoundedId::new(&format!("cred:{hex}"))
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
    requirement: Option<&LaunchRequirement>,
) -> Option<ProviderAttempt> {
    let requirement = requirement?;
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
        // Set by the send site that knows the exact bytes; a binding made
        // before the body exists honestly records that it does not know them.
        body_digest: None,
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
        // Both are set by the send site, which is the only place that knows
        // the exact endpoint and the exact credential material in use.
        route_digest: None,
        credential_digest: None,
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
        // Everything past the transport boundary whose outcome nobody
        // observed is exactly ambiguous, and that includes a delivered
        // request whose answer was never read.
        SendState::Sending | SendState::Sent | SendState::Responding => {
            attempt.advance(SendState::Uncertain)
        }
        // A request that never left is still provably unsent, a settled one
        // is finished, and an already-fenced one stays fenced; none of the
        // three needs reconciling here.
        SendState::KnownNotSent | SendState::Settled | SendState::Uncertain => Ok(()),
    }
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
