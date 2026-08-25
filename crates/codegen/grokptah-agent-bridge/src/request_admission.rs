//! One atomic admission per physical provider request.
//!
//! # What this replaces
//!
//! The previous revision admitted a *turn*: the gate ran once, recorded what
//! the session's model and effort were at that instant, and then the transport
//! independently re-resolved credentials, provider profile, base endpoint, and
//! model just before sending — and did so again on every retry and after every
//! 401 refresh. Nothing tied the three readings together, so the durable record
//! described the first one while the provider received the third.
//!
//! Here the resolution happens exactly once, in [`admit_call`], and produces an
//! [`AdmittedCall`] holding the **exact bytes** that will go out. The transport
//! no longer resolves anything; it reads bytes from the carrier and sends them.
//!
//! # Why the type does the enforcing
//!
//! [`crate::host_helpers::call_xai_agent_step`] and its sibling take an
//! `&AdmittedCall` and have no other way to learn a URL, a credential, or a
//! body. "Send without admission" is therefore a signature that does not
//! typecheck, rather than a rule reviewers have to keep noticing. The
//! `no_unadmitted_provider_calls` conformance test covers the residue the type
//! system cannot: a fresh `reqwest` client built somewhere new.
//!
//! # One admission is one HTTP request
//!
//! A retry is a *new* physical request with its own delivery question, so it
//! gets its own [`grokptah_agent_sdk::attempt::ProviderAttempt`] with its own
//! ordinal and its own idempotency key. Reusing one attempt across a retry
//! loop — as the previous revision did — makes the ledger say "one request"
//! where five were sent.

use anyhow::{anyhow, Result};
use grokptah_agent_sdk::attempt::{
    AttemptIntent, AttemptSubject, AuthorityRevisions, BoundedId, ProviderAttempt,
    ProviderReceipts, SendState,
};
use grokptah_agent_sdk::launch::{LaunchReason, ModelReference};
use grokptah_agent_sdk::resolved::{
    EndpointIdentity, RequestBinding, ResolvedRequest, ResolvedRequestParts,
};
use uuid::Uuid;

use crate::attempt_binding;
use crate::auth_store::WireCredentials;
use crate::host_helpers::ResolvedModelTarget;
use crate::orchestration::OrchStore;
use crate::types::EffortLevel;

/// The revision of the host source that produced a binding.
///
/// Compiled in rather than read at runtime so a record cannot claim to have
/// been produced by a build other than the one that produced it.
pub(crate) fn source_revision() -> BoundedId {
    BoundedId::new(option_env!("GROKPTAH_SOURCE_REVISION").unwrap_or("src:unversioned"))
        .unwrap_or_else(|| BoundedId::new("src:unversioned").expect("literal is bounded"))
}

/// Who and where a call acts for, and where its attempts are recorded.
///
/// The ledger is not optional. A physical provider request that cannot be
/// recorded is a request nobody can reconcile afterwards, so it is refused
/// rather than sent unrecorded — which is the opposite of the previous
/// revision, where desktop persistence was best-effort and a ledger failure
/// let the turn carry on regardless.
#[derive(Clone)]
pub struct CallProvenance {
    /// The durable run these attempts belong to.
    pub run_id: String,
    /// The owning session.
    pub session_id: Uuid,
    /// The approved workspace, reduced to an opaque handle before binding.
    pub workspace: String,
    /// Opaque tenant identity, when one is established.
    pub tenant: Option<String>,
    /// Opaque project identity, when one is established.
    pub project: Option<String>,
    /// Where attempts are recorded. Required.
    pub ledger: OrchStore,
    /// The authority revisions this call is decided under.
    pub authority: AuthorityRevisions,
}

/// What a caller wants the provider to do.
///
/// Deliberately the *whole* request: the previous revision digested the prompt
/// alone, leaving the system preamble, the history, the tool declarations, the
/// model, and the effort outside the binding, all of which change what is
/// asked and what it costs.
pub(crate) struct CallIntent<'a> {
    /// Model selection, in `provider/model` form.
    pub model_selection: &'a str,
    /// Reasoning effort for this call.
    pub effort: EffortLevel,
    /// The complete message list, including any system preamble and history.
    pub messages: &'a [serde_json::Value],
    /// Tool declarations, when this call offers any.
    pub tools: Option<&'a serde_json::Value>,
    /// Whether this call streams.
    pub stream: bool,
    /// Whether this call offers `tool_choice: auto`.
    ///
    /// Some compatible gateways accept native tools but reject the optional
    /// `tool_choice` field. Narrowing the request is a *different* request, so
    /// it is re-admitted and re-sealed rather than edited in place — which is
    /// what the previous revision did, leaving the ledger describing a body
    /// that was never sent.
    pub tool_choice: bool,
}

/// One admitted physical call: exact bytes, plus what is needed to send them.
///
/// The credentials are held here rather than in the [`ResolvedRequest`] on
/// purpose: the carrier is the thing whose binding gets persisted, and a
/// bearer must never be one edit away from a durable record.
pub(crate) struct AdmittedCall {
    request: ResolvedRequest,
    credentials: WireCredentials,
    target: ResolvedModelTarget,
    provenance: CallProvenance,
    /// Ordinals already consumed by this call's own retries.
    next_ordinal: u32,
}

impl AdmittedCall {
    /// The exact bytes to transmit.
    pub fn body(&self) -> &[u8] {
        self.request.body()
    }

    /// The persistable binding for these bytes.
    pub fn binding(&self) -> &RequestBinding {
        self.request.binding()
    }

    /// Credentials for the authorization header. Never persisted.
    pub fn credentials(&self) -> &WireCredentials {
        &self.credentials
    }

    /// The resolved route: base URL, dialect, capabilities, deadline class.
    pub fn target(&self) -> &ResolvedModelTarget {
        &self.target
    }

    /// Re-verify the carried bytes against the digest they were sealed under.
    pub fn verify_intact(&self) -> Result<()> {
        self.request.verify_intact().map_err(|error| anyhow!(error))
    }

    /// Replace the credentials after a provider-forced refresh.
    ///
    /// The refreshed credential is a *different* credential, so the binding's
    /// `credential_revision` advances with it and the next attempt records
    /// that it was sent under the rotated one. The request bytes are
    /// untouched: a refresh changes who is asking, never what is asked.
    pub fn rotate_credentials(&mut self, refreshed: WireCredentials) -> Result<()> {
        let parts = rebind_parts(
            self.request.binding(),
            self.request.binding().credential_revision.saturating_add(1),
        );
        let body = self.request.body().to_vec();
        self.request = ResolvedRequest::seal(parts, body).map_err(|error| anyhow!(error))?;
        self.credentials = refreshed;
        Ok(())
    }

    /// Open and persist the next attempt for this call, in `known_not_sent`.
    ///
    /// Persisting *before* the request can leave means a crash between here
    /// and dispatch leaves the only state that is safe to retry. A ledger
    /// write failure refuses the send.
    pub fn open_attempt(&mut self) -> Result<ProviderAttempt> {
        let ordinal = self.next_ordinal;
        self.next_ordinal = self.next_ordinal.saturating_add(1);

        let recorded = self
            .provenance
            .ledger
            .list_attempts_for_run(&self.provenance.run_id)
            .map_err(|error| anyhow!("could not read the attempt ledger: {error}"))?;
        if !attempt_binding::permits_new_request(&recorded) {
            return Err(anyhow!(
                "an earlier provider attempt for this run is still unreconciled; \
                 reconcile it against its idempotency key before issuing another request"
            ));
        }
        let ordinal = attempt_binding::next_ordinal(&recorded).max(ordinal);

        let binding = self.request.binding();
        let attempt = ProviderAttempt::open(
            attempt_binding::attempt_id(&self.provenance.run_id, ordinal),
            BoundedId::new(&self.provenance.run_id)
                .unwrap_or_else(|| attempt_binding::opaque_handle("run", &self.provenance.run_id)),
            ordinal,
            binding.subject.clone(),
            binding.authority,
            binding.attempt_route(),
            AttemptIntent {
                digest: binding.digest.as_bounded(),
                request_id: attempt_binding::opaque_handle("req", &self.provenance.run_id),
                provider_idempotency_key: attempt_binding::provider_idempotency_key(
                    &self.provenance.run_id,
                    ordinal,
                ),
            },
        );
        self.provenance
            .ledger
            .open_attempt(&attempt)
            .map_err(|error| anyhow!("could not record the provider attempt: {error}"))?;
        Ok(attempt)
    }

    /// Move an attempt to `sending`, immediately before the request leaves.
    ///
    /// Fails closed: if this write does not land, the caller must not send,
    /// because a delivered request with no `sending` record on disk is exactly
    /// the duplicate-charge case the ledger exists to prevent.
    pub fn begin_send(&self, attempt: &ProviderAttempt) -> Result<()> {
        self.provenance
            .ledger
            .update_attempt(attempt.attempt_id.as_str(), |attempt| {
                attempt
                    .advance(SendState::Sending)
                    .map_err(anyhow::Error::msg)
            })
            .map_err(|error| anyhow!("could not record the send boundary: {error}"))?
            .ok_or_else(|| anyhow!("the provider attempt disappeared before it was sent"))?;
        Ok(())
    }

    /// Settle an attempt the provider answered.
    pub fn settle_sent(&self, attempt: &ProviderAttempt, receipts: ProviderReceipts) -> Result<()> {
        self.provenance
            .ledger
            .update_attempt(attempt.attempt_id.as_str(), |attempt| {
                attempt.receipts = receipts.clone();
                attempt.advance(SendState::Sent).map_err(anyhow::Error::msg)
            })
            .map_err(|error| anyhow!("could not settle the provider attempt: {error}"))?;
        Ok(())
    }

    /// Settle an attempt whose outcome cannot be established.
    pub fn settle_uncertain(
        &self,
        attempt: &ProviderAttempt,
        failure: grokptah_agent_sdk::outcome::RunFailureKind,
    ) -> Result<()> {
        self.provenance
            .ledger
            .update_attempt(attempt.attempt_id.as_str(), |attempt| {
                attempt
                    .advance(SendState::Uncertain)
                    .map_err(anyhow::Error::msg)?;
                attempt.failure = Some(failure);
                Ok(())
            })
            .map_err(|error| anyhow!("could not settle the provider attempt: {error}"))?;
        Ok(())
    }

    /// Attach reported usage to one already-settled attempt.
    ///
    /// The send state is untouched: this records what the provider said it
    /// consumed, which is a different fact from whether the request arrived.
    pub fn attach_usage(
        &self,
        attempt_id: &BoundedId,
        usage: grokptah_agent_sdk::attempt::UsageReceipt,
    ) -> Result<()> {
        self.provenance
            .ledger
            .update_attempt(attempt_id.as_str(), |attempt| {
                attempt.receipts.usage = Some(usage);
                Ok(())
            })
            .map_err(|error| anyhow!("could not record reported usage: {error}"))?;
        Ok(())
    }

    /// Whether another physical request may be issued for this call.
    ///
    /// False as soon as any recorded attempt needs provider-side
    /// reconciliation, which is what stops a retry loop from duplicating a
    /// request that may already have run.
    pub fn permits_another_request(&self) -> Result<bool> {
        self.provenance
            .ledger
            .run_permits_new_attempt(&self.provenance.run_id)
            .map_err(|error| anyhow!("could not read the attempt ledger: {error}"))
    }
}

/// Rebuild sealing parts from an existing binding, at a new credential revision.
fn rebind_parts(binding: &RequestBinding, credential_revision: u64) -> ResolvedRequestParts {
    ResolvedRequestParts {
        subject: binding.subject.clone(),
        authority: binding.authority,
        provider: binding.provider,
        profile: binding.profile.clone(),
        endpoint: binding.endpoint.clone(),
        route: binding.route,
        dialect: binding.dialect,
        model: binding.model.clone(),
        effort: binding.effort.clone(),
        credential_method: binding.credential_method,
        credential_revision,
        account_reference: binding.account_reference.clone(),
        source_revision: binding.source_revision.clone(),
    }
}

/// Resolve, enforce, and seal one physical provider call.
///
/// Everything the request depends on is read exactly once, here: the
/// credential, the provider profile, the base endpoint, the wire model, the
/// dialect, the capabilities, and the effort. The body is then built from those
/// resolved values and sealed, so what the ledger describes and what the
/// provider receives cannot diverge.
pub(crate) async fn admit_call(
    intent: CallIntent<'_>,
    provenance: CallProvenance,
) -> std::result::Result<AdmittedCall, LaunchReason> {
    // 1. Resolve the credential for the *selected* model, once.
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(intent.model_selection)
        .map_err(|_| LaunchReason::CredentialRouteUnrecognized)?
        .ok_or(LaunchReason::SignInRequired)?;
    let credentials = crate::auth_store::ensure_fresh_credentials(credentials).await;

    // 2. Enforce launch truth against that exact credential, once.
    let truth = crate::launch_truth::re_resolve_launch_truth(
        intent.model_selection,
        chrono::Utc::now().timestamp(),
    )
    .await;
    let facts = crate::launch_truth::enforce(truth, None)?;

    // 3. Resolve the exact route, once. From here nothing re-resolves.
    let target = crate::host_helpers::resolve_model_target(&credentials, intent.model_selection)
        .map_err(|_| LaunchReason::ModelNotSelected)?;
    if intent.tools.is_some() && !target.capabilities.tools {
        return Err(LaunchReason::CapabilitiesUnprobed);
    }

    // 4. Build the exact body from the resolved values.
    let mut body = serde_json::json!({
        "model": target.wire_model,
        "messages": intent.messages,
        "stream": intent.stream && target.capabilities.stream,
    });
    if let Some(tools) = intent.tools {
        body["tools"] = tools.clone();
        if intent.tool_choice {
            body["tool_choice"] = serde_json::json!("auto");
        }
    }
    // A reasoning effort the provider has never been qualified for is an
    // unprobed capability, not a request to send and hope.
    crate::host_helpers::apply_effort_to_agent_body(&mut body, &target, intent.effort)
        .map_err(|_| LaunchReason::CapabilitiesUnprobed)?;
    let bytes = serde_json::to_vec(&body).map_err(|_| LaunchReason::CredentialRouteUnrecognized)?;

    // 5. Seal. The digest is computed from these bytes, never supplied.
    let parts = ResolvedRequestParts {
        subject: AttemptSubject {
            principal: facts
                .requirement
                .account_reference
                .as_ref()
                .and_then(|reference| BoundedId::new(&reference.value)),
            tenant: provenance.tenant.as_deref().and_then(BoundedId::new),
            project: provenance.project.as_deref().and_then(BoundedId::new),
            workspace: attempt_binding::workspace_handle(&provenance.workspace),
            session: attempt_binding::opaque_handle("ses", &provenance.session_id.to_string()),
        },
        authority: provenance.authority,
        provider: facts.requirement.provider,
        // The exact selected profile, not one inferred from the family.
        profile: BoundedId::new(&credentials.provider_id)
            .unwrap_or_else(|| attempt_binding::opaque_handle("pf", &credentials.provider_id)),
        endpoint: EndpointIdentity::of_base_url(facts.requirement.base, &target.base_url),
        route: facts.requirement.route,
        dialect: facts.requirement.dialect,
        model: ModelReference::new(&target.wire_model)
            .ok_or(LaunchReason::ModelSelectionUnparseable)?,
        effort: BoundedId::new(intent.effort.as_str())
            .ok_or(LaunchReason::ModelSelectionUnparseable)?,
        credential_method: facts.requirement.credential_method,
        credential_revision: crate::auth_store::credential_revision(&credentials),
        account_reference: facts.requirement.account_reference.clone(),
        source_revision: source_revision(),
    };
    let request = ResolvedRequest::seal(parts, bytes)
        .map_err(|_| LaunchReason::CredentialRouteUnrecognized)?;

    Ok(AdmittedCall {
        request,
        credentials,
        target,
        provenance,
        next_ordinal: 1,
    })
}

/// Where a session's calls are attributed and recorded, for the duration of a
/// turn.
///
/// # Why a registry rather than another parameter
///
/// A provider call can originate deep inside a turn — a subagent, a
/// qualification probe, a compaction summary — where threading a run id and a
/// ledger handle through every intermediate signature would mean touching code
/// that has nothing to do with authority, and would leave each of those
/// signatures free to pass `None`.
///
/// The registry inverts that: the host registers provenance when it opens a
/// turn, and any call made under that session finds it. A call made with **no**
/// registration cannot be admitted at all, so the failure mode of forgetting to
/// register is a refused send rather than an unrecorded one.
pub mod registry {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    use uuid::Uuid;

    use super::CallProvenance;

    #[allow(clippy::type_complexity)]
    static ACTIVE: OnceLock<Mutex<HashMap<Uuid, CallProvenance>>> = OnceLock::new();

    fn table() -> &'static Mutex<HashMap<Uuid, CallProvenance>> {
        ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    /// Register provenance for a session's in-flight turn.
    ///
    /// Returns a guard that deregisters on drop, so a turn that panics does
    /// not leave a stale run id that a later call could attach itself to.
    pub fn register(session_id: Uuid, provenance: CallProvenance) -> Guard {
        let mut guard = match table().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let previous = guard.insert(session_id, provenance);
        Guard {
            session_id,
            previous,
        }
    }

    /// The provenance a call under this session must record against.
    pub(crate) fn lookup(session_id: Uuid) -> Option<CallProvenance> {
        let guard = match table().lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.get(&session_id).cloned()
    }

    /// Restores whatever provenance was in place before, on drop.
    ///
    /// Nested turns (a subagent inside a turn) therefore see their own
    /// provenance while they run and hand the parent's back afterwards.
    pub struct Guard {
        session_id: Uuid,
        previous: Option<CallProvenance>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            let mut guard = match table().lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match self.previous.take() {
                Some(previous) => {
                    guard.insert(self.session_id, previous);
                }
                None => {
                    guard.remove(&self.session_id);
                }
            }
        }
    }
}

/// Admit one call on behalf of a session, using its registered provenance.
///
/// Fails closed when the session has no provenance registered: a provider
/// request that cannot be attributed to a run and recorded in a ledger is
/// refused rather than sent unrecorded.
pub(crate) async fn admit_for_session(
    session_id: Uuid,
    intent: CallIntent<'_>,
) -> std::result::Result<AdmittedCall, LaunchReason> {
    let Some(provenance) = registry::lookup(session_id) else {
        return Err(LaunchReason::SignInRequired);
    };
    admit_call(intent, provenance).await
}
