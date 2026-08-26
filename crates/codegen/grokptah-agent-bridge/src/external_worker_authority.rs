//! Production authority for external-worker launch, follow-up, and cancel.
//!
//! A provider-neutral adapter is a transport, not an authority. This module is
//! the gate in front of it. Nothing reaches a provider unless, in order:
//!
//! 1. the capability is advertised — a qualified adapter is registered,
//!    answered a bounded reachability probe, speaks this contract version, and
//!    host policy allows it;
//! 2. this host itself minted an admission naming the exact principal,
//!    session, workspace, run, mutation, provider, capability revision,
//!    payload digest, target, and lifetime, and that admission is unspent and
//!    unexpired *at send time*, not merely at mint time;
//! 3. the durable ledger has no receipt or tombstone saying the same intent
//!    already happened or is still unresolved.
//!
//! After a send, the only three outcomes are accepted, rejected with no
//! provider effect, or `Uncertain`. Uncertain is sticky: it blocks automatic
//! *and* explicit retry until a reconciliation decision says what the provider
//! actually did. That asymmetry is deliberate — a duplicated cloud agent costs
//! real money and real writes, so ambiguity must stop the lane rather than
//! resolve itself optimistically.

use std::collections::BTreeSet;
use std::sync::Arc;

use grokptah_agent_sdk::{
    ExternalWorkerAdmission, ExternalWorkerCapabilityStatus, ExternalWorkerFollowUpRequest,
    ExternalWorkerLaunchRequest, ExternalWorkerLaunchResult, ExternalWorkerMutation,
    ExternalWorkerProvider, ExternalWorkerReceipt, ExternalWorkerReceiptState,
    ExternalWorkerRecord, ExternalWorkerRunRecord, ExternalWorkerScope, ExternalWorkerTarget,
    EXTERNAL_WORKER_CONTRACT_VERSION, MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS,
};
use serde_json::json;

use crate::external_worker::{ExternalWorkerAdapterError, ExternalWorkerRegistry};
use crate::external_worker_store::{AdmissionState, ExternalWorkerStore, MutationClaim};

/// Default lifetime minted into an external-worker admission.
pub const DEFAULT_ADMISSION_TTL_MS: u64 = 2 * 60 * 1_000;

/// A monotonic wall-clock source, injectable so authority tests are exact.
pub trait ExternalWorkerClock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

/// The process clock used outside tests.
pub struct SystemClock;

impl ExternalWorkerClock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as u64)
            .unwrap_or_default()
    }
}

/// Host policy deciding which principals and providers may mutate at all.
///
/// Policy is a separate gate from registration on purpose: installing an
/// adapter is a deployment fact, while allowing a principal to spend it is an
/// authority decision that can be withdrawn without uninstalling anything.
#[derive(Debug, Clone, Default)]
pub struct ExternalWorkerPolicy {
    allowed_providers: BTreeSet<(ExternalWorkerProvider, Option<String>)>,
    allowed_workspaces: BTreeSet<String>,
    allowed_principals: BTreeSet<String>,
    mutations_enabled: bool,
}

impl ExternalWorkerPolicy {
    /// A policy that denies everything.
    pub fn deny_all() -> Self {
        Self::default()
    }

    /// Allow one exact provider identity.
    pub fn allow_provider(
        mut self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
    ) -> Self {
        self.allowed_providers
            .insert((provider, provider_id.map(str::to_owned)));
        self
    }

    /// Allow one workspace alias.
    pub fn allow_workspace(mut self, workspace: &str) -> Self {
        self.allowed_workspaces.insert(workspace.to_owned());
        self
    }

    /// Allow one authenticated principal.
    pub fn allow_principal(mut self, principal_id: &str) -> Self {
        self.allowed_principals.insert(principal_id.to_owned());
        self
    }

    /// Enable mutations for the identities this policy already allows.
    pub fn enable_mutations(mut self) -> Self {
        self.mutations_enabled = true;
        self
    }

    /// Whether this provider identity may be advertised or spent at all.
    pub fn allows_provider(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
    ) -> bool {
        self.mutations_enabled
            && self
                .allowed_providers
                .contains(&(provider, provider_id.map(str::to_owned)))
    }

    fn check_scope(&self, scope: &ExternalWorkerScope) -> Result<(), ExternalWorkerAdapterError> {
        if !self.mutations_enabled {
            return Err(ExternalWorkerAdapterError::Unavailable(
                "external-worker mutations are disabled by host policy",
            ));
        }
        if !self.allowed_principals.contains(&scope.principal_id) {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "principal is not allowed to mutate external workers",
            ));
        }
        if !self.allowed_workspaces.contains(&scope.workspace) {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "workspace is not allowed to mutate external workers",
            ));
        }
        Ok(())
    }
}

/// Everything a caller must present before an admission can be minted.
#[derive(Debug, Clone)]
pub struct AdmissionRequest {
    /// Exact principal/session/workspace/run fence.
    pub scope: ExternalWorkerScope,
    /// The single mutation to authorize.
    pub mutation: ExternalWorkerMutation,
    /// Provider family to authorize.
    pub provider: ExternalWorkerProvider,
    /// Adapter identity for custom providers.
    pub provider_id: Option<String>,
    /// Caller idempotency key for the mutation.
    pub request_id: String,
    /// Exact bounded payload the admission is minted for.
    pub payload: MutationPayload,
    /// Requested lifetime; clamped to the host ceiling.
    pub ttl_ms: u64,
}

/// The exact bounded payload one mutation carries.
///
/// Digesting the payload rather than storing it keeps prompts and provider
/// bodies out of the durable ledger while still making a replay with different
/// content detectable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationPayload {
    /// Create an isolated worker and its initial run.
    Launch(Box<ExternalWorkerLaunchRequest>),
    /// Queue one bounded follow-up run on an existing worker.
    FollowUp {
        /// Opaque provider worker the follow-up targets.
        external_agent_id: String,
        /// The bounded follow-up prompt and limits.
        request: Box<ExternalWorkerFollowUpRequest>,
    },
    /// Cancel one active provider run.
    Cancel {
        /// Opaque provider worker owning the run.
        external_agent_id: String,
        /// Opaque provider run to cancel.
        external_run_id: String,
    },
}

impl MutationPayload {
    /// The mutation kind this payload can only ever be spent as.
    pub fn mutation(&self) -> ExternalWorkerMutation {
        match self {
            Self::Launch(_) => ExternalWorkerMutation::Launch,
            Self::FollowUp { .. } => ExternalWorkerMutation::FollowUp,
            Self::Cancel { .. } => ExternalWorkerMutation::Cancel,
        }
    }

    /// The opaque provider target this payload names, if any.
    pub fn target(&self) -> Option<ExternalWorkerTarget> {
        match self {
            Self::Launch(_) => None,
            Self::FollowUp {
                external_agent_id, ..
            } => Some(ExternalWorkerTarget {
                external_agent_id: external_agent_id.clone(),
                external_run_id: None,
            }),
            Self::Cancel {
                external_agent_id,
                external_run_id,
            } => Some(ExternalWorkerTarget {
                external_agent_id: external_agent_id.clone(),
                external_run_id: Some(external_run_id.clone()),
            }),
        }
    }

    /// Validate the caller payload before anything is minted for it.
    pub fn validate(&self) -> Result<(), ExternalWorkerAdapterError> {
        match self {
            Self::Launch(request) => request
                .validate()
                .map_err(ExternalWorkerAdapterError::InvalidRequest),
            Self::FollowUp { request, .. } => request
                .validate()
                .map_err(ExternalWorkerAdapterError::InvalidRequest),
            Self::Cancel { .. } => Ok(()),
        }?;
        if let Some(target) = self.target() {
            target
                .validate()
                .map_err(ExternalWorkerAdapterError::InvalidRequest)?;
        }
        Ok(())
    }

    /// `sha256:<hex>` digest binding the exact bounded payload.
    ///
    /// The digest covers the mutation kind and target as well as the body, so
    /// a follow-up payload can never collide with a cancel payload that
    /// happens to serialize alike.
    pub fn digest(&self) -> String {
        let value = match self {
            Self::Launch(request) => json!({
                "mutation": ExternalWorkerMutation::Launch.as_str(),
                "request": request,
            }),
            Self::FollowUp {
                external_agent_id,
                request,
            } => json!({
                "mutation": ExternalWorkerMutation::FollowUp.as_str(),
                "externalAgentId": external_agent_id,
                "request": request,
            }),
            Self::Cancel {
                external_agent_id,
                external_run_id,
            } => json!({
                "mutation": ExternalWorkerMutation::Cancel.as_str(),
                "externalAgentId": external_agent_id,
                "externalRunId": external_run_id,
            }),
        };
        format!("sha256:{}", crate::orchestration::hash_payload(&value))
    }
}

/// One accepted mutation and the redacted receipt that proves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedMutation<T> {
    /// The verified provider projection.
    pub value: T,
    /// The durable redacted receipt for this mutation.
    pub receipt: ExternalWorkerReceipt,
}

/// The gate in front of every external-worker mutation.
pub struct ExternalWorkerAuthority {
    registry: Arc<ExternalWorkerRegistry>,
    store: ExternalWorkerStore,
    policy: ExternalWorkerPolicy,
    clock: Arc<dyn ExternalWorkerClock>,
    nonce_source: Arc<dyn Fn() -> String + Send + Sync>,
}

impl ExternalWorkerAuthority {
    /// Build an authority over an explicit registry, ledger, and policy.
    pub fn new(
        registry: Arc<ExternalWorkerRegistry>,
        store: ExternalWorkerStore,
        policy: ExternalWorkerPolicy,
    ) -> Self {
        Self {
            registry,
            store,
            policy,
            clock: Arc::new(SystemClock),
            nonce_source: Arc::new(|| format!("ewn-{}", uuid::Uuid::new_v4())),
        }
    }

    /// Replace the clock; used by deterministic authority tests.
    pub fn with_clock(mut self, clock: Arc<dyn ExternalWorkerClock>) -> Self {
        self.clock = clock;
        self
    }

    /// Replace the nonce source; used by deterministic authority tests.
    pub fn with_nonce_source(
        mut self,
        nonce_source: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        self.nonce_source = nonce_source;
        self
    }

    /// The durable ledger backing this authority.
    pub fn store(&self) -> &ExternalWorkerStore {
        &self.store
    }

    /// The provider registry this authority derives capability truth from.
    pub fn registry(&self) -> &ExternalWorkerRegistry {
        &self.registry
    }

    /// Capability truth for one provider identity, policy included.
    pub async fn capability_status(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
    ) -> ExternalWorkerCapabilityStatus {
        self.registry
            .capability_status(
                provider,
                provider_id,
                self.policy.allows_provider(provider, provider_id),
            )
            .await
    }

    /// Capability truth for every installed provider identity.
    pub async fn capability_report(&self) -> Vec<ExternalWorkerCapabilityStatus> {
        let mut report = Vec::new();
        for (provider, provider_id) in self.registry.provider_keys() {
            report.push(
                self.capability_status(provider, provider_id.as_deref())
                    .await,
            );
        }
        report
    }

    /// Mint a scope-bound, single-use admission for exactly one mutation.
    ///
    /// Minting is itself gated: an admission is never issued for a capability
    /// the host would not advertise, so a caller cannot obtain a ticket it
    /// could never legally spend.
    pub async fn mint_admission(
        &self,
        request: AdmissionRequest,
    ) -> Result<ExternalWorkerAdmission, ExternalWorkerAdapterError> {
        request
            .scope
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        self.policy.check_scope(&request.scope)?;
        request.payload.validate()?;
        if request.payload.mutation() != request.mutation {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "payload does not match the requested mutation",
            ));
        }
        let status = self
            .capability_status(request.provider, request.provider_id.as_deref())
            .await;
        if !status.is_available() {
            return Err(ExternalWorkerAdapterError::Unavailable(
                "external-worker capability is not advertised for this provider",
            ));
        }

        let now_ms = self.clock.now_ms();
        let ttl_ms = request
            .ttl_ms
            .clamp(1, MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS);
        let nonce = (self.nonce_source)();
        let admission = ExternalWorkerAdmission {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
            admission_id: format!("adm-{nonce}"),
            nonce,
            request_id: request.request_id,
            scope: request.scope,
            mutation: request.mutation,
            provider: request.provider,
            provider_id: request.provider_id,
            capability_revision: status.capability_revision,
            issued_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
            payload_digest: request.payload.digest(),
            target: request.payload.target(),
        };
        admission
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        self.store.record_admission(&admission, now_ms)?;
        Ok(admission)
    }

    /// Withdraw an unspent admission.
    pub fn revoke_admission(&self, nonce: &str) -> Result<(), ExternalWorkerAdapterError> {
        self.store.revoke_admission(nonce, self.clock.now_ms())
    }

    /// Launch an isolated external worker under a host-minted admission.
    pub async fn launch(
        &self,
        admission: &ExternalWorkerAdmission,
        request: &ExternalWorkerLaunchRequest,
    ) -> Result<AdmittedMutation<ExternalWorkerLaunchResult>, ExternalWorkerAdapterError> {
        let payload = MutationPayload::Launch(Box::new(request.clone()));
        let (adapter, receipt) = self
            .admit(admission, ExternalWorkerMutation::Launch, &payload)
            .await?;
        let sent = adapter.launch(request).await;
        let target = sent.as_ref().ok().map(|result| ExternalWorkerTarget {
            external_agent_id: result.worker.external_agent_id.clone(),
            external_run_id: Some(result.run.external_run_id.clone()),
        });
        self.settle(receipt, sent, target)
    }

    /// Queue a bounded follow-up run under a host-minted admission.
    pub async fn follow_up(
        &self,
        admission: &ExternalWorkerAdmission,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> Result<AdmittedMutation<ExternalWorkerRunRecord>, ExternalWorkerAdapterError> {
        let payload = MutationPayload::FollowUp {
            external_agent_id: external_agent_id.to_owned(),
            request: Box::new(request.clone()),
        };
        let (adapter, receipt) = self
            .admit(admission, ExternalWorkerMutation::FollowUp, &payload)
            .await?;
        let sent = adapter.follow_up(external_agent_id, request).await;
        let target = sent.as_ref().ok().map(|run| ExternalWorkerTarget {
            external_agent_id: run.external_agent_id.clone(),
            external_run_id: Some(run.external_run_id.clone()),
        });
        self.settle(receipt, sent, target)
    }

    /// Cancel one active provider run under a host-minted admission.
    pub async fn cancel(
        &self,
        admission: &ExternalWorkerAdmission,
        external_agent_id: &str,
        external_run_id: &str,
    ) -> Result<AdmittedMutation<ExternalWorkerRunRecord>, ExternalWorkerAdapterError> {
        let payload = MutationPayload::Cancel {
            external_agent_id: external_agent_id.to_owned(),
            external_run_id: external_run_id.to_owned(),
        };
        let (adapter, receipt) = self
            .admit(admission, ExternalWorkerMutation::Cancel, &payload)
            .await?;
        let sent = adapter.cancel(external_agent_id, external_run_id).await;
        let target = sent.as_ref().ok().map(|run| ExternalWorkerTarget {
            external_agent_id: run.external_agent_id.clone(),
            external_run_id: Some(run.external_run_id.clone()),
        });
        self.settle(receipt, sent, target)
    }

    /// Read a redacted worker projection. This is not a mutation and needs no
    /// admission, but it is still bounded by capability truth.
    pub async fn get_worker(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
        external_agent_id: &str,
    ) -> Result<ExternalWorkerRecord, ExternalWorkerAdapterError> {
        self.available_adapter(provider, provider_id)
            .await?
            .get_worker(external_agent_id)
            .await
    }

    /// Resolve an uncertain receipt with an explicit decision.
    ///
    /// This is the only exit from `Uncertain`. A caller supplies the provider
    /// target when reconciling to accepted, so the permanent tombstone records
    /// which provider object the ambiguous send actually created.
    pub fn reconcile(
        &self,
        request_id: &str,
        resolved: ExternalWorkerReceiptState,
        target: Option<ExternalWorkerTarget>,
        reason: &str,
    ) -> Result<ExternalWorkerReceipt, ExternalWorkerAdapterError> {
        self.store
            .reconcile_mutation(request_id, resolved, target, reason, self.clock.now_ms())
    }

    /// Read the durable receipt for one idempotency key.
    pub fn receipt(
        &self,
        request_id: &str,
    ) -> Result<Option<ExternalWorkerReceipt>, ExternalWorkerAdapterError> {
        self.store.load_receipt(request_id)
    }

    /// Revalidate an admission and claim the right to send exactly once.
    async fn admit(
        &self,
        admission: &ExternalWorkerAdmission,
        expected: ExternalWorkerMutation,
        payload: &MutationPayload,
    ) -> Result<
        (
            Arc<dyn crate::external_worker::ExternalWorkerAdapter>,
            ExternalWorkerReceipt,
        ),
        ExternalWorkerAdapterError,
    > {
        // Shape first: a malformed ticket never reaches the ledger.
        admission
            .validate()
            .map_err(ExternalWorkerAdapterError::AdmissionRejected)?;
        payload.validate()?;
        if admission.mutation != expected || payload.mutation() != expected {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not authorize this mutation",
            ));
        }
        if admission.payload_digest != payload.digest() {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not bind this exact payload",
            ));
        }
        if admission.target != payload.target() {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not bind this provider target",
            ));
        }
        self.policy.check_scope(&admission.scope)?;

        // Ledger second: only an admission this host minted is authority, and
        // only if every stored binding still matches what is presented.
        let record = self.store.load_admission(&admission.nonce)?.ok_or(
            ExternalWorkerAdapterError::AdmissionRejected("admission was not minted by this host"),
        )?;
        if &record.admission != admission {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not match the host mint record",
            ));
        }
        // Single use is checked explicitly rather than inferred from the
        // receipt ledger, so a spent ticket stays dead even if its receipt was
        // reconciled, replayed, or pruned.
        if record.state != AdmissionState::Minted {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission has already been spent or revoked",
            ));
        }

        // Time third: expiry is checked at send time, not at mint time.
        let now_ms = self.clock.now_ms();
        if !admission.is_live_at(now_ms) {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission is expired or not yet valid",
            ));
        }

        // Capability fourth: the revision the ticket was minted against must
        // still be the live one, and the capability must still be advertised.
        let status = self
            .capability_status(admission.provider, admission.provider_id.as_deref())
            .await;
        if !status.is_available() {
            return Err(ExternalWorkerAdapterError::Unavailable(
                "external-worker capability is no longer advertised",
            ));
        }
        if status.capability_revision != admission.capability_revision {
            return Err(ExternalWorkerAdapterError::AdmissionRejected(
                "admission was minted against a stale capability revision",
            ));
        }

        let adapter = self
            .registry
            .get(admission.provider, admission.provider_id.as_deref())
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)?;

        // Idempotency fifth: a receipt or tombstone can still refuse the send.
        let receipt = ExternalWorkerReceipt {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.to_owned(),
            request_id: admission.request_id.clone(),
            admission_id: admission.admission_id.clone(),
            mutation: admission.mutation,
            scope: admission.scope.clone(),
            provider: admission.provider,
            provider_id: admission.provider_id.clone(),
            provider_request_id: provider_request_id(admission),
            attempt: 1,
            state: ExternalWorkerReceiptState::Claimed,
            target: None,
            payload_digest: admission.payload_digest.clone(),
            reason: "admitted and claimed for one provider send".to_owned(),
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        match self.store.claim_mutation(&receipt)? {
            MutationClaim::Perform(claimed) => {
                // The nonce is burned only once the send is genuinely ours, so
                // a request refused by the ledger does not consume the ticket.
                if let Err(error) = self.store.spend_admission(&admission.nonce, now_ms) {
                    // Nothing has left this host yet, so the claim is settled
                    // as a clean rejection rather than left in flight: a
                    // receipt abandoned in `Claimed` would reopen as
                    // `Uncertain` and wedge the lane over a send that never
                    // happened.
                    self.store.settle_mutation(&ExternalWorkerReceipt {
                        state: ExternalWorkerReceiptState::Rejected,
                        reason: "mutation was refused with no provider effect".to_owned(),
                        ..claimed
                    })?;
                    return Err(error);
                }
                Ok((adapter, claimed))
            }
            MutationClaim::Pending(_) => Err(ExternalWorkerAdapterError::Conflict(
                "an identical external-worker mutation is already in flight",
            )),
            MutationClaim::Uncertain(_) => Err(ExternalWorkerAdapterError::Uncertain(
                "an earlier attempt on this request has an unknown provider outcome",
            )),
            MutationClaim::Replay(_) => Err(ExternalWorkerAdapterError::Conflict(
                "this external-worker mutation was already accepted by the provider",
            )),
            MutationClaim::Rejected(_) => Err(ExternalWorkerAdapterError::Conflict(
                "this external-worker mutation already settled as rejected",
            )),
        }
    }

    /// Record the outcome of one send, then hand back the value or the error.
    fn settle<T>(
        &self,
        claimed: ExternalWorkerReceipt,
        sent: Result<T, ExternalWorkerAdapterError>,
        target: Option<ExternalWorkerTarget>,
    ) -> Result<AdmittedMutation<T>, ExternalWorkerAdapterError> {
        let now_ms = self.clock.now_ms().max(claimed.updated_at_ms);
        let mut receipt = ExternalWorkerReceipt {
            updated_at_ms: now_ms,
            ..claimed
        };
        match sent {
            Ok(value) => {
                let Some(target) = target else {
                    // A success we cannot attribute to a provider object is an
                    // ambiguity, not a success: something exists out there
                    // that this host cannot name.
                    receipt.state = ExternalWorkerReceiptState::Uncertain;
                    receipt.reason =
                        "provider accepted the mutation without a nameable target".to_owned();
                    self.store.settle_mutation(&receipt)?;
                    return Err(ExternalWorkerAdapterError::Uncertain(
                        "provider accepted the mutation without a nameable target",
                    ));
                };
                receipt.state = ExternalWorkerReceiptState::Accepted;
                receipt.target = Some(target);
                receipt.reason = "provider accepted the admitted mutation".to_owned();
                self.store.settle_mutation(&receipt)?;
                Ok(AdmittedMutation { value, receipt })
            }
            Err(error) => {
                // A failed send never yields an accepted disposition, so the
                // receipt keeps no target: a mutation this host cannot name
                // must not look like one it can.
                let state = post_send_disposition(&error);
                debug_assert!(!state.is_accepted());
                receipt.state = state;
                receipt.reason = disposition_reason(state).to_owned();
                self.store.settle_mutation(&receipt)?;
                if state == ExternalWorkerReceiptState::Uncertain {
                    return Err(ExternalWorkerAdapterError::Uncertain(
                        "the provider outcome is unknown and must be reconciled",
                    ));
                }
                Err(error)
            }
        }
    }

    async fn available_adapter(
        &self,
        provider: ExternalWorkerProvider,
        provider_id: Option<&str>,
    ) -> Result<Arc<dyn crate::external_worker::ExternalWorkerAdapter>, ExternalWorkerAdapterError>
    {
        if !self
            .capability_status(provider, provider_id)
            .await
            .is_available()
        {
            return Err(ExternalWorkerAdapterError::Unavailable(
                "external-worker capability is not advertised for this provider",
            ));
        }
        self.registry
            .get(provider, provider_id)
            .ok_or(ExternalWorkerAdapterError::UnsupportedProvider)
    }
}

/// Derive the stable provider-facing request identity for one admission.
///
/// It is a pure function of the admitted intent, so every attempt on the same
/// request presents the provider with the same identity. That is what lets a
/// reconciled retry be recognized by the provider as the same request rather
/// than as a second one.
pub fn provider_request_id(admission: &ExternalWorkerAdmission) -> String {
    let value = json!({
        "contract": EXTERNAL_WORKER_CONTRACT_VERSION,
        "mutation": admission.mutation.as_str(),
        "scope": admission.scope,
        "requestId": admission.request_id,
        "payloadDigest": admission.payload_digest,
    });
    let digest = crate::orchestration::hash_payload(&value);
    format!("ewp-{}", &digest[..40])
}

/// Classify an adapter error into a durable receipt disposition.
///
/// The dividing line is whether provider state could have changed. Anything
/// this host rejected before transport is a clean rejection; anything that may
/// have reached the provider — including a response this host could not verify
/// — is uncertain, because a response proves the provider acted.
pub fn post_send_disposition(error: &ExternalWorkerAdapterError) -> ExternalWorkerReceiptState {
    match error {
        // Refused by this host or definitively by the provider: no effect.
        ExternalWorkerAdapterError::InvalidRequest(_)
        | ExternalWorkerAdapterError::UnsupportedProvider
        | ExternalWorkerAdapterError::InvalidBaseUrl
        | ExternalWorkerAdapterError::ProviderAlreadyRegistered
        | ExternalWorkerAdapterError::AdmissionRejected(_)
        | ExternalWorkerAdapterError::Unavailable(_) => ExternalWorkerReceiptState::Rejected,
        ExternalWorkerAdapterError::Provider { status } => {
            if status.is_client_error() && !matches!(status.as_u16(), 408 | 409 | 425 | 429) {
                ExternalWorkerReceiptState::Rejected
            } else {
                ExternalWorkerReceiptState::Uncertain
            }
        }
        // A response arrived but could not be verified, the transport failed
        // mid-flight, or durable state failed: provider effect is unknown.
        _ => ExternalWorkerReceiptState::Uncertain,
    }
}

fn disposition_reason(state: ExternalWorkerReceiptState) -> &'static str {
    match state {
        ExternalWorkerReceiptState::Accepted => "provider accepted the admitted mutation",
        ExternalWorkerReceiptState::Rejected => "mutation was refused with no provider effect",
        ExternalWorkerReceiptState::Uncertain => {
            "provider outcome is unknown; retry is blocked until reconciled"
        }
        ExternalWorkerReceiptState::Claimed => "admitted and claimed for one provider send",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_worker::test_support::{FakeAdapter, FakeOutcome};
    use crate::external_worker_store::AdmissionState;
    use grokptah_agent_sdk::{ExternalWorkerExecutionMode, ExternalWorkerState};
    use parking_lot::Mutex as ParkingMutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    const GATEWAY: &str = "gateway-a";

    /// A clock the tests move by hand, so expiry and retention are exact.
    struct FixedClock(AtomicU64);

    impl FixedClock {
        fn new(now_ms: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(now_ms)))
        }

        fn set(&self, now_ms: u64) {
            self.0.store(now_ms, Ordering::SeqCst);
        }
    }

    impl ExternalWorkerClock for FixedClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn scope() -> ExternalWorkerScope {
        ExternalWorkerScope {
            principal_id: "principal-1".into(),
            session_id: "session-1".into(),
            workspace: "grokptah-main".into(),
            run_id: "run-1".into(),
        }
    }

    fn policy() -> ExternalWorkerPolicy {
        ExternalWorkerPolicy::deny_all()
            .allow_provider(ExternalWorkerProvider::Custom, Some(GATEWAY))
            .allow_workspace("grokptah-main")
            .allow_principal("principal-1")
            .enable_mutations()
    }

    fn launch_request(request_id: &str, prompt: &str) -> ExternalWorkerLaunchRequest {
        ExternalWorkerLaunchRequest {
            request_id: request_id.into(),
            provider: ExternalWorkerProvider::Custom,
            provider_id: Some(GATEWAY.into()),
            repository: "chriscase/GrokPtah".into(),
            starting_ref: "refs/heads/codex/review".into(),
            prompt: prompt.into(),
            model: None,
            execution_mode: ExternalWorkerExecutionMode::Isolated,
            auto_create_pr: false,
            bounds: None,
        }
    }

    struct Harness {
        authority: ExternalWorkerAuthority,
        adapter: Arc<FakeAdapter>,
        clock: Arc<FixedClock>,
        _root: tempfile::TempDir,
    }

    fn harness_with(
        outcomes: Vec<FakeOutcome>,
        reachable: bool,
        policy: ExternalWorkerPolicy,
    ) -> Harness {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let clock = FixedClock::new(1_000_000);
        let registry = Arc::new(ExternalWorkerRegistry::new());
        let adapter = Arc::new(FakeAdapter::custom(GATEWAY, reachable).script(outcomes));
        registry
            .register(adapter.clone())
            .expect("adapter installs once");
        let store = ExternalWorkerStore::open(root.path(), clock.now_ms()).expect("ledger opens");
        let counter = Arc::new(ParkingMutex::new(0u64));
        let authority = ExternalWorkerAuthority::new(registry, store, policy)
            .with_clock(clock.clone())
            .with_nonce_source(Arc::new(move || {
                let mut next = counter.lock();
                *next += 1;
                format!("nonce-{next}")
            }));
        Harness {
            authority,
            adapter,
            clock,
            _root: root,
        }
    }

    fn harness(outcomes: Vec<FakeOutcome>) -> Harness {
        harness_with(outcomes, true, policy())
    }

    async fn mint_launch(
        harness: &Harness,
        request: &ExternalWorkerLaunchRequest,
    ) -> ExternalWorkerAdmission {
        harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Launch,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: request.request_id.clone(),
                payload: MutationPayload::Launch(Box::new(request.clone())),
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("admission mints")
    }

    async fn mint_follow_up(
        harness: &Harness,
        external_agent_id: &str,
        request: &ExternalWorkerFollowUpRequest,
    ) -> ExternalWorkerAdmission {
        harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::FollowUp,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: request.request_id.clone(),
                payload: MutationPayload::FollowUp {
                    external_agent_id: external_agent_id.to_owned(),
                    request: Box::new(request.clone()),
                },
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("follow-up admission mints")
    }

    // ---------------------------------------------------------------- admission

    #[tokio::test]
    async fn a_well_formed_admission_this_host_never_minted_fails_closed() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let mut forged = mint_launch(&harness, &request).await;
        forged.nonce = "nonce-forged".into();
        forged.admission_id = "adm-nonce-forged".into();
        forged
            .validate()
            .expect("the forgery is perfectly well formed");

        let error = harness
            .authority
            .launch(&forged, &request)
            .await
            .expect_err("an unminted admission must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected("admission was not minted by this host")
        ));
        assert_eq!(harness.adapter.sends(), 0, "nothing may reach the provider");
        assert!(harness.authority.receipt("req-1").unwrap().is_none());
    }

    #[tokio::test]
    async fn an_edited_admission_no_longer_matches_the_mint_record() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;

        for edit in [0usize, 1, 2, 3, 4] {
            let mut tampered = minted.clone();
            match edit {
                0 => tampered.scope.principal_id = "principal-2".into(),
                1 => tampered.scope.session_id = "session-2".into(),
                2 => tampered.scope.workspace = "grokptah-other".into(),
                3 => tampered.scope.run_id = "run-2".into(),
                _ => tampered.capability_revision += 1,
            }
            let error = harness
                .authority
                .launch(&tampered, &request)
                .await
                .expect_err("an edited admission must fail closed");
            // A changed workspace/principal is refused by policy before the
            // ledger is consulted; everything else fails the mint comparison.
            assert!(
                matches!(error, ExternalWorkerAdapterError::AdmissionRejected(_)),
                "edit {edit} produced {error:?}"
            );
        }
        assert_eq!(harness.adapter.sends(), 0);
    }

    #[tokio::test]
    async fn an_admission_cannot_be_spent_on_a_different_payload_or_mutation() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;

        let swapped = launch_request("req-1", "exfiltrate the workspace instead");
        let error = harness
            .authority
            .launch(&minted, &swapped)
            .await
            .expect_err("a swapped payload must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not bind this exact payload"
            )
        ));

        let error = harness
            .authority
            .cancel(&minted, "fake-agent", "fake-run-1")
            .await
            .expect_err("a launch ticket must not buy a cancel");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission does not authorize this mutation"
            )
        ));
        assert_eq!(harness.adapter.sends(), 0);
    }

    #[tokio::test]
    async fn an_expired_admission_fails_closed_at_send_time() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;
        assert!(minted.is_live_at(harness.clock.now_ms()));

        // The ticket was live when it was minted and is not live now.
        harness.clock.set(minted.expires_at_ms);
        let error = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect_err("an expired admission must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected("admission is expired or not yet valid")
        ));
        assert_eq!(harness.adapter.sends(), 0);
    }

    #[tokio::test]
    async fn a_stale_capability_revision_fails_closed() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;
        assert_eq!(minted.capability_revision, 1);

        // A second adapter installs, moving the capability revision. Tickets
        // minted against the old adapter set are no longer spendable.
        harness
            .authority
            .registry()
            .register(Arc::new(FakeAdapter::custom("gateway-b", true)))
            .expect("second identity installs");
        let error = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect_err("a stale revision must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission was minted against a stale capability revision"
            )
        ));
        assert_eq!(harness.adapter.sends(), 0);
    }

    #[tokio::test]
    async fn a_spent_admission_is_never_accepted_twice() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;

        let accepted = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect("the first launch is admitted");
        assert_eq!(accepted.receipt.state, ExternalWorkerReceiptState::Accepted);
        assert_eq!(
            harness
                .authority
                .store()
                .load_admission(&minted.nonce)
                .unwrap()
                .expect("mint record")
                .state,
            AdmissionState::Spent
        );

        // Replaying the same ticket is refused by the single-use nonce.
        let error = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect_err("a spent ticket must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission has already been spent or revoked"
            )
        ));

        // A freshly minted ticket for the same intent is refused by the
        // durable acceptance record instead, so both defences are load-bearing.
        let fresh = mint_launch(&harness, &request).await;
        let error = harness
            .authority
            .launch(&fresh, &request)
            .await
            .expect_err("a duplicate launch must fail closed");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::Conflict(
                "this external-worker mutation was already accepted by the provider"
            )
        ));
        assert_eq!(harness.adapter.sends(), 1, "exactly one provider send");
    }

    #[tokio::test]
    async fn policy_and_capability_gates_block_minting_entirely() {
        // Policy refuses the principal.
        let harness = harness_with(
            vec![FakeOutcome::Accept],
            true,
            ExternalWorkerPolicy::deny_all()
                .allow_provider(ExternalWorkerProvider::Custom, Some(GATEWAY))
                .allow_workspace("grokptah-main")
                .allow_principal("someone-else")
                .enable_mutations(),
        );
        let request = launch_request("req-1", "review the exact candidate");
        let error = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Launch,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-1".into(),
                payload: MutationPayload::Launch(Box::new(request.clone())),
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect_err("a disallowed principal must not receive a ticket");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(
                "principal is not allowed to mutate external workers"
            )
        ));

        // The adapter is installed but unreachable, so nothing is advertised.
        let harness = harness_with(vec![FakeOutcome::Accept], false, policy());
        let error = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Launch,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-1".into(),
                payload: MutationPayload::Launch(Box::new(request)),
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect_err("an unreachable adapter must not be admitted");
        assert!(matches!(error, ExternalWorkerAdapterError::Unavailable(_)));
        assert_eq!(harness.adapter.sends(), 0);
    }

    #[tokio::test]
    async fn an_unsupported_provider_identity_is_never_admitted() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let error = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Launch,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some("gateway-unknown".into()),
                request_id: "req-1".into(),
                payload: MutationPayload::Launch(Box::new(launch_request("req-1", "hello"))),
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect_err("an unregistered identity must not be admitted");
        assert!(matches!(error, ExternalWorkerAdapterError::Unavailable(_)));
    }

    #[tokio::test]
    async fn admission_ttl_is_clamped_to_the_host_ceiling() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Launch,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-1".into(),
                payload: MutationPayload::Launch(Box::new(request)),
                ttl_ms: u64::MAX,
            })
            .await
            .expect("an over-long request is clamped, not refused");
        assert_eq!(
            minted.expires_at_ms - minted.issued_at_ms,
            MAX_EXTERNAL_WORKER_ADMISSION_TTL_MS
        );
        minted.validate().expect("clamped admission is valid");
    }

    // ------------------------------------------------------- follow-up / cancel

    #[tokio::test]
    async fn duplicate_follow_up_and_cancel_fail_closed_after_acceptance() {
        let harness = harness(vec![FakeOutcome::Accept, FakeOutcome::Accept]);
        let follow_up = ExternalWorkerFollowUpRequest {
            request_id: "req-follow".into(),
            prompt: "tighten the bounded check".into(),
            bounds: None,
        };
        let follow_admission = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::FollowUp,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-follow".into(),
                payload: MutationPayload::FollowUp {
                    external_agent_id: "fake-agent".into(),
                    request: Box::new(follow_up.clone()),
                },
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("follow-up admission mints");
        let accepted = harness
            .authority
            .follow_up(&follow_admission, "fake-agent", &follow_up)
            .await
            .expect("first follow-up is admitted");
        assert_eq!(accepted.value.state, ExternalWorkerState::Running);
        assert_eq!(harness.adapter.follow_ups.load(Ordering::SeqCst), 1);

        // Ticket reuse is refused by the single-use nonce...
        assert!(matches!(
            harness
                .authority
                .follow_up(&follow_admission, "fake-agent", &follow_up)
                .await
                .expect_err("duplicate follow-up on a spent ticket"),
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission has already been spent or revoked"
            )
        ));
        // ...and a fresh ticket for the same intent by the durable receipt.
        let replacement = mint_follow_up(&harness, "fake-agent", &follow_up).await;
        assert!(matches!(
            harness
                .authority
                .follow_up(&replacement, "fake-agent", &follow_up)
                .await
                .expect_err("duplicate follow-up on a fresh ticket"),
            ExternalWorkerAdapterError::Conflict(_)
        ));
        assert_eq!(harness.adapter.follow_ups.load(Ordering::SeqCst), 1);

        let cancel_admission = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Cancel,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-cancel".into(),
                payload: MutationPayload::Cancel {
                    external_agent_id: "fake-agent".into(),
                    external_run_id: "fake-run-2".into(),
                },
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("cancel admission mints");
        let cancelled = harness
            .authority
            .cancel(&cancel_admission, "fake-agent", "fake-run-2")
            .await
            .expect("first cancel is admitted");
        assert_eq!(cancelled.value.state, ExternalWorkerState::Cancelled);
        assert!(matches!(
            harness
                .authority
                .cancel(&cancel_admission, "fake-agent", "fake-run-2")
                .await
                .expect_err("duplicate cancel on a spent ticket"),
            ExternalWorkerAdapterError::AdmissionRejected(
                "admission has already been spent or revoked"
            )
        ));
        let replacement = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::Cancel,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-cancel".into(),
                payload: MutationPayload::Cancel {
                    external_agent_id: "fake-agent".into(),
                    external_run_id: "fake-run-2".into(),
                },
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("a replacement cancel ticket mints");
        assert!(matches!(
            harness
                .authority
                .cancel(&replacement, "fake-agent", "fake-run-2")
                .await
                .expect_err("duplicate cancel on a fresh ticket"),
            ExternalWorkerAdapterError::Conflict(_)
        ));
        assert_eq!(harness.adapter.cancels.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_follow_up_ticket_is_bound_to_its_exact_worker() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let follow_up = ExternalWorkerFollowUpRequest {
            request_id: "req-follow".into(),
            prompt: "tighten the bounded check".into(),
            bounds: None,
        };
        let admission = harness
            .authority
            .mint_admission(AdmissionRequest {
                scope: scope(),
                mutation: ExternalWorkerMutation::FollowUp,
                provider: ExternalWorkerProvider::Custom,
                provider_id: Some(GATEWAY.into()),
                request_id: "req-follow".into(),
                payload: MutationPayload::FollowUp {
                    external_agent_id: "fake-agent".into(),
                    request: Box::new(follow_up.clone()),
                },
                ttl_ms: DEFAULT_ADMISSION_TTL_MS,
            })
            .await
            .expect("follow-up admission mints");

        let error = harness
            .authority
            .follow_up(&admission, "someone-elses-agent", &follow_up)
            .await
            .expect_err("a ticket for one worker must not target another");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::AdmissionRejected(_)
        ));
        assert_eq!(harness.adapter.follow_ups.load(Ordering::SeqCst), 0);
    }

    // ------------------------------------------------------------- uncertainty

    #[tokio::test]
    async fn an_ambiguous_send_is_uncertain_and_blocks_every_retry() {
        let harness = harness(vec![FakeOutcome::AmbiguousAfterSend, FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;

        let error = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect_err("an ambiguous send must not look successful");
        assert!(matches!(error, ExternalWorkerAdapterError::Uncertain(_)));
        assert_eq!(harness.adapter.sends(), 1);

        let receipt = harness
            .authority
            .receipt("req-1")
            .unwrap()
            .expect("an uncertain receipt is durable");
        assert_eq!(receipt.state, ExternalWorkerReceiptState::Uncertain);
        assert!(receipt.state.blocks_retry());
        assert!(
            harness
                .authority
                .store()
                .load_tombstone("req-1")
                .unwrap()
                .is_none(),
            "an unresolved send must not claim provider effect"
        );

        // An explicit retry with a brand new admission is still refused: the
        // block is on the request identity, not on the ticket.
        let fresh = mint_launch(&harness, &request).await;
        let error = harness
            .authority
            .launch(&fresh, &request)
            .await
            .expect_err("explicit retry must stay blocked");
        assert!(matches!(error, ExternalWorkerAdapterError::Uncertain(_)));
        assert_eq!(harness.adapter.sends(), 1, "no second provider send");
    }

    #[tokio::test]
    async fn reconciling_to_accepted_writes_a_tombstone_and_keeps_retry_blocked() {
        let harness = harness(vec![FakeOutcome::AmbiguousAfterSend, FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;
        let _ = harness.authority.launch(&minted, &request).await;

        let reconciled = harness
            .authority
            .reconcile(
                "req-1",
                ExternalWorkerReceiptState::Accepted,
                Some(ExternalWorkerTarget {
                    external_agent_id: "fake-agent".into(),
                    external_run_id: Some("fake-run-1".into()),
                }),
                "operator read the provider and found the worker",
            )
            .expect("uncertain receipts reconcile");
        assert_eq!(reconciled.state, ExternalWorkerReceiptState::Accepted);
        let tombstone = harness
            .authority
            .store()
            .load_tombstone("req-1")
            .unwrap()
            .expect("acceptance is now permanent");
        assert_eq!(tombstone.target.external_agent_id, "fake-agent");

        let fresh = mint_launch(&harness, &request).await;
        assert!(matches!(
            harness
                .authority
                .launch(&fresh, &request)
                .await
                .expect_err("an accepted mutation never re-sends"),
            ExternalWorkerAdapterError::Conflict(_)
        ));
        assert_eq!(harness.adapter.sends(), 1);
    }

    #[tokio::test]
    async fn reconciling_to_rejected_releases_the_lane_for_one_fresh_attempt() {
        let harness = harness(vec![FakeOutcome::AmbiguousAfterSend, FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;
        let _ = harness.authority.launch(&minted, &request).await;

        harness
            .authority
            .reconcile(
                "req-1",
                ExternalWorkerReceiptState::Rejected,
                None,
                "operator read the provider and found no worker",
            )
            .expect("uncertain receipts reconcile");

        // A settled rejection is still a durable answer for this request id,
        // so the same intent is not silently re-sent under the old key.
        let fresh = mint_launch(&harness, &request).await;
        assert!(matches!(
            harness
                .authority
                .launch(&fresh, &request)
                .await
                .expect_err("a settled rejection is still an answer"),
            ExternalWorkerAdapterError::Conflict(_)
        ));

        // A new caller intent gets a new idempotency key and does send.
        let retry = launch_request("req-2", "review the exact candidate");
        let admission = mint_launch(&harness, &retry).await;
        let accepted = harness
            .authority
            .launch(&admission, &retry)
            .await
            .expect("a fresh intent is admitted");
        assert_eq!(accepted.receipt.state, ExternalWorkerReceiptState::Accepted);
        assert_eq!(harness.adapter.sends(), 2);
    }

    #[tokio::test]
    async fn reconciliation_only_applies_to_uncertain_receipts() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;
        harness
            .authority
            .launch(&minted, &request)
            .await
            .expect("accepted");

        assert!(matches!(
            harness
                .authority
                .reconcile("req-1", ExternalWorkerReceiptState::Rejected, None, "no")
                .expect_err("an accepted receipt is not reconcilable"),
            ExternalWorkerAdapterError::Conflict("only an uncertain receipt can be reconciled")
        ));
        assert!(matches!(
            harness
                .authority
                .reconcile("req-404", ExternalWorkerReceiptState::Accepted, None, "no")
                .expect_err("an unknown receipt is not reconcilable"),
            ExternalWorkerAdapterError::Conflict("no receipt to reconcile")
        ));
        assert!(matches!(
            harness
                .authority
                .reconcile("req-1", ExternalWorkerReceiptState::Uncertain, None, "no")
                .expect_err("uncertain is not a resolution"),
            ExternalWorkerAdapterError::Conflict(
                "reconciliation must settle as accepted or rejected"
            )
        ));
    }

    #[tokio::test]
    async fn a_rejection_before_send_leaves_the_lane_clean() {
        let harness = harness(vec![FakeOutcome::RejectBeforeSend]);
        let request = launch_request("req-1", "review the exact candidate");
        let minted = mint_launch(&harness, &request).await;

        let error = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect_err("the provider refused");
        assert!(matches!(
            error,
            ExternalWorkerAdapterError::InvalidRequest(_)
        ));
        let receipt = harness
            .authority
            .receipt("req-1")
            .unwrap()
            .expect("receipt");
        assert_eq!(receipt.state, ExternalWorkerReceiptState::Rejected);
        assert!(!receipt.state.blocks_retry());
        assert!(harness
            .authority
            .store()
            .load_tombstone("req-1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn post_send_disposition_treats_any_possible_effect_as_uncertain() {
        use reqwest::StatusCode;
        for (error, expected) in [
            (
                ExternalWorkerAdapterError::InvalidRequest("bad"),
                ExternalWorkerReceiptState::Rejected,
            ),
            (
                ExternalWorkerAdapterError::UnsupportedProvider,
                ExternalWorkerReceiptState::Rejected,
            ),
            (
                ExternalWorkerAdapterError::AdmissionRejected("bad"),
                ExternalWorkerReceiptState::Rejected,
            ),
            (
                ExternalWorkerAdapterError::Provider {
                    status: StatusCode::BAD_REQUEST,
                },
                ExternalWorkerReceiptState::Rejected,
            ),
            (
                ExternalWorkerAdapterError::Provider {
                    status: StatusCode::TOO_MANY_REQUESTS,
                },
                ExternalWorkerReceiptState::Uncertain,
            ),
            (
                ExternalWorkerAdapterError::Provider {
                    status: StatusCode::CONFLICT,
                },
                ExternalWorkerReceiptState::Uncertain,
            ),
            (
                ExternalWorkerAdapterError::Provider {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                },
                ExternalWorkerReceiptState::Uncertain,
            ),
            (
                // A response arrived that this host could not verify: the
                // provider acted, so effect is unknown, not absent.
                ExternalWorkerAdapterError::InvalidResponse("unverifiable"),
                ExternalWorkerReceiptState::Uncertain,
            ),
            (
                ExternalWorkerAdapterError::Durable("ledger".into()),
                ExternalWorkerReceiptState::Uncertain,
            ),
        ] {
            assert_eq!(
                post_send_disposition(&error),
                expected,
                "{error:?} classified wrongly"
            );
        }
    }

    // ------------------------------------------------------------------ restart

    #[tokio::test]
    async fn a_restart_reopens_an_in_flight_receipt_as_uncertain() {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let claimed = ExternalWorkerReceipt {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.into(),
            request_id: "req-1".into(),
            admission_id: "adm-nonce-1".into(),
            mutation: ExternalWorkerMutation::Launch,
            scope: scope(),
            provider: ExternalWorkerProvider::Custom,
            provider_id: Some(GATEWAY.into()),
            provider_request_id: "ewp-stable".into(),
            attempt: 1,
            state: ExternalWorkerReceiptState::Claimed,
            target: None,
            payload_digest: MutationPayload::Launch(Box::new(launch_request("req-1", "p")))
                .digest(),
            reason: "admitted and claimed for one provider send".into(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        };
        {
            let store = ExternalWorkerStore::open(root.path(), 1_000).expect("ledger opens");
            assert!(matches!(
                store.claim_mutation(&claimed).unwrap(),
                MutationClaim::Perform(_)
            ));
            // The process stops here, with the send in flight.
        }

        let store = ExternalWorkerStore::open(root.path(), 5_000).expect("ledger reopens");
        let reopened = store
            .load_receipt("req-1")
            .unwrap()
            .expect("the receipt survived the restart");
        assert_eq!(reopened.state, ExternalWorkerReceiptState::Uncertain);
        assert!(store.load_tombstone("req-1").unwrap().is_none());
        assert!(matches!(
            store.claim_mutation(&claimed).unwrap(),
            MutationClaim::Uncertain(_)
        ));
    }

    #[tokio::test]
    async fn an_accepted_tombstone_survives_restart_and_receipt_pruning() {
        let root = tempfile::tempdir().expect("temporary ledger root");
        let payload = MutationPayload::Launch(Box::new(launch_request("req-1", "p")));
        let accepted = ExternalWorkerReceipt {
            contract: EXTERNAL_WORKER_CONTRACT_VERSION.into(),
            request_id: "req-1".into(),
            admission_id: "adm-nonce-1".into(),
            mutation: ExternalWorkerMutation::Launch,
            scope: scope(),
            provider: ExternalWorkerProvider::Custom,
            provider_id: Some(GATEWAY.into()),
            provider_request_id: "ewp-stable".into(),
            attempt: 1,
            state: ExternalWorkerReceiptState::Claimed,
            target: None,
            payload_digest: payload.digest(),
            reason: "admitted and claimed for one provider send".into(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        };
        {
            let store = ExternalWorkerStore::open(root.path(), 1_000).expect("ledger opens");
            store.claim_mutation(&accepted).unwrap();
            store
                .settle_mutation(&ExternalWorkerReceipt {
                    state: ExternalWorkerReceiptState::Accepted,
                    target: Some(ExternalWorkerTarget {
                        external_agent_id: "fake-agent".into(),
                        external_run_id: Some("fake-run-1".into()),
                    }),
                    reason: "provider accepted the admitted mutation".into(),
                    updated_at_ms: 2_000,
                    ..accepted.clone()
                })
                .unwrap();
            assert_eq!(store.receipt_count().unwrap(), 1);
            assert_eq!(store.tombstone_count().unwrap(), 1);
        }

        // Restart far past the receipt retention horizon: the settled receipt
        // is pruned, the acceptance record is not.
        let much_later = 2_000 + 30 * 24 * 60 * 60 * 1_000;
        let store = ExternalWorkerStore::open(root.path(), much_later).expect("ledger reopens");
        assert_eq!(store.receipt_count().unwrap(), 0, "receipt was pruned");
        assert_eq!(
            store.tombstone_count().unwrap(),
            1,
            "tombstone is permanent"
        );
        let tombstone = store
            .load_tombstone("req-1")
            .unwrap()
            .expect("acceptance is permanent");
        assert_eq!(tombstone.provider_request_id, "ewp-stable");

        // A duplicate of the same intent still replays instead of re-sending.
        assert!(matches!(
            store.claim_mutation(&accepted).unwrap(),
            MutationClaim::Replay(_)
        ));
        // A different intent under the same key is a hard conflict.
        let different = ExternalWorkerReceipt {
            payload_digest: MutationPayload::Launch(Box::new(launch_request("req-1", "other")))
                .digest(),
            ..accepted
        };
        assert!(matches!(
            store.claim_mutation(&different),
            Err(ExternalWorkerAdapterError::Conflict(_))
        ));
    }

    // -------------------------------------------------------------- projections

    #[tokio::test]
    async fn provider_request_identity_is_stable_across_attempts() {
        let harness = harness(vec![FakeOutcome::AmbiguousAfterSend]);
        let request = launch_request("req-1", "review the exact candidate");
        let first = mint_launch(&harness, &request).await;
        let _ = harness.authority.launch(&first, &request).await;
        let uncertain = harness
            .authority
            .receipt("req-1")
            .unwrap()
            .expect("receipt");

        // A different ticket for the same intent derives the same provider
        // identity, so a reconciled retry is the same request to the provider.
        let second = mint_launch(&harness, &request).await;
        assert_ne!(first.nonce, second.nonce);
        assert_eq!(provider_request_id(&first), provider_request_id(&second));
        assert_eq!(uncertain.provider_request_id, provider_request_id(&second));

        // A different payload or scope derives a different provider identity.
        let other_payload = launch_request("req-1", "a different prompt entirely");
        let other = mint_launch(&harness, &other_payload).await;
        assert_ne!(provider_request_id(&first), provider_request_id(&other));
    }

    #[tokio::test]
    async fn admissions_and_receipts_carry_no_privileged_material() {
        let harness = harness(vec![FakeOutcome::Accept]);
        let request = launch_request(
            "req-1",
            "read https://api.cursor.com with Authorization: Bearer secret-token",
        );
        let minted = mint_launch(&harness, &request).await;
        let accepted = harness
            .authority
            .launch(&minted, &request)
            .await
            .expect("accepted");

        for value in [
            serde_json::to_string(&minted).expect("admission serializes"),
            serde_json::to_string(&accepted.receipt).expect("receipt serializes"),
        ] {
            let lower = value.to_ascii_lowercase();
            for needle in [
                "bearer",
                "authorization",
                "secret-token",
                "api.cursor.com",
                "https://",
                "read https",
                "/users/",
            ] {
                assert!(
                    !lower.contains(needle),
                    "projection leaked {needle:?}: {value}"
                );
            }
        }
        assert!(grokptah_agent_sdk::validate_digest(&minted.payload_digest).is_ok());
        accepted.receipt.validate().expect("receipt is publishable");
    }

    #[tokio::test]
    async fn capability_report_projects_every_installed_identity() {
        let harness = harness(vec![FakeOutcome::Accept]);
        harness
            .authority
            .registry()
            .register(Arc::new(FakeAdapter::custom("gateway-unlisted", true)))
            .expect("second identity installs");

        let report = harness.authority.capability_report().await;
        assert_eq!(report.len(), 2);
        for status in &report {
            status.validate().expect("status is publishable");
        }
        let allowed = report
            .iter()
            .find(|status| status.provider_id.as_deref() == Some(GATEWAY))
            .expect("the allowed identity is reported");
        assert!(allowed.is_available());
        let unlisted = report
            .iter()
            .find(|status| status.provider_id.as_deref() == Some("gateway-unlisted"))
            .expect("the unlisted identity is reported");
        assert!(!unlisted.is_available());
        assert_eq!(
            unlisted.reason.as_deref(),
            Some("host policy does not allow this provider")
        );
    }
}
