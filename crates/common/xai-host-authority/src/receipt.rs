//! The one receipt family.
//!
//! Every gate hands back a value from this module; there is no second,
//! parallel receipt type anywhere in the spine. Each receipt carries the full
//! binding tuple, so a receipt minted for one principal, session, workspace,
//! resource incarnation, control epoch, observation revision, or action digest
//! cannot be presented for another.
//!
//! None of these types is constructible outside this crate: fields are
//! private and the constructors are `pub(crate)`. That is what "no
//! caller-forgeable approvals" means structurally rather than by convention.

use crate::digest::ContentDigest;
use crate::ids::{
    AttemptId, AuthGeneration, CapabilityGeneration, CapabilityId, ControlEpoch,
    CredentialIncarnation, EffectLeaseId, ObservationRevision, PolicyRevision, PrincipalId,
    ResourceIncarnation, SessionId, WorkspaceId,
};

/// The complete authority binding tuple.
///
/// Carried by every receipt. Equality is total: two bindings match only when
/// every component matches, so a cross-principal, cross-session, or
/// cross-workspace presentation is a mismatch rather than a near-miss.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct AuthorityBinding {
    pub(crate) principal: PrincipalId,
    pub(crate) incarnation: CredentialIncarnation,
    pub(crate) auth_generation: AuthGeneration,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) policy_revision: PolicyRevision,
    pub(crate) session: SessionId,
    pub(crate) workspace: WorkspaceId,
    pub(crate) resource: ResourceIncarnation,
    pub(crate) control_epoch: ControlEpoch,
}

impl AuthorityBinding {
    pub fn principal(&self) -> PrincipalId {
        self.principal
    }
    pub fn session(&self) -> SessionId {
        self.session
    }
    pub fn workspace(&self) -> WorkspaceId {
        self.workspace
    }
    pub fn resource(&self) -> ResourceIncarnation {
        self.resource
    }
    pub fn control_epoch(&self) -> ControlEpoch {
        self.control_epoch
    }
    pub fn capability_generation(&self) -> CapabilityGeneration {
        self.capability_generation
    }
    pub fn policy_revision(&self) -> PolicyRevision {
        self.policy_revision
    }
}

impl std::fmt::Debug for AuthorityBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Opaque handles only — safe in logs and MCP projections.
        f.debug_struct("AuthorityBinding")
            .field("principal", &self.principal.public_handle())
            .field("session", &self.session.public_handle())
            .field("workspace", &self.workspace.public_handle())
            .field("resource", &self.resource.public_handle())
            .finish_non_exhaustive()
    }
}

/// Gate 1 receipt: proof that the host authenticated this principal *now*.
///
/// Minted only by [`crate::HostAuthority::authenticate`] against a durable
/// credential record whose fingerprint matched the presented secret.
#[derive(Clone, PartialEq, Eq)]
pub struct AuthContext {
    pub(crate) principal: PrincipalId,
    pub(crate) incarnation: CredentialIncarnation,
    pub(crate) auth_generation: AuthGeneration,
    pub(crate) capability_generation: CapabilityGeneration,
    pub(crate) policy_revision: PolicyRevision,
    pub(crate) control_epoch: ControlEpoch,
    pub(crate) credential_id: String,
    pub(crate) owner_id: String,
}

impl AuthContext {
    pub fn principal(&self) -> PrincipalId {
        self.principal
    }
    pub fn auth_generation(&self) -> AuthGeneration {
        self.auth_generation
    }
    pub fn capability_generation(&self) -> CapabilityGeneration {
        self.capability_generation
    }
    pub fn policy_revision(&self) -> PolicyRevision {
        self.policy_revision
    }
    pub fn control_epoch(&self) -> ControlEpoch {
        self.control_epoch
    }
    pub fn credential_id(&self) -> &str {
        &self.credential_id
    }
    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }
    /// Public, secret-free projection of who this is.
    pub fn public_handle(&self) -> String {
        self.principal.public_handle()
    }
}

impl std::fmt::Debug for AuthContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthContext")
            .field("principal", &self.principal.public_handle())
            .field("credential_id", &self.credential_id)
            .finish_non_exhaustive()
    }
}

/// Gate 2 receipt: a sealed capability grant.
///
/// "Sealed" means the grant's scope was fixed by the host at issue time and
/// cannot be widened afterwards — there is no setter, and no public
/// constructor to build a wider one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SealedCapability {
    pub(crate) id: CapabilityId,
    pub(crate) binding: AuthorityBinding,
    pub(crate) actor: ActorClass,
    pub(crate) effect: EffectClass,
    pub(crate) expires_at_ms: u64,
}

impl SealedCapability {
    pub fn id(&self) -> CapabilityId {
        self.id
    }
    pub fn binding(&self) -> &AuthorityBinding {
        &self.binding
    }
    pub fn effect(&self) -> EffectClass {
        self.effect
    }
    /// Who stands behind this grant. Read-only: there is no setter, so a
    /// model-sealed capability can never be re-presented as operator-sealed.
    pub fn actor(&self) -> ActorClass {
        self.actor
    }
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
}

/// Who stands behind a capability.
///
/// Required when the capability is sealed, so there is no absent-actor case
/// that could be read as operator authority by default. A model-originated
/// proposal cannot present itself as operator-approved: the host fixes the
/// actor at seal time and nothing widens it afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum ActorClass {
    /// A human operator the host verified.
    VerifiedOperator,
    /// A model proposal the host admitted. Never equivalent to operator
    /// authority, however the proposal was phrased.
    VerifiedModel,
}

impl ActorClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::VerifiedOperator => "verified_operator",
            Self::VerifiedModel => "verified_model",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "verified_operator" => Some(Self::VerifiedOperator),
            "verified_model" => Some(Self::VerifiedModel),
            _ => None,
        }
    }

    /// Whether this actor carries operator authority.
    pub const fn is_operator(self) -> bool {
        matches!(self, Self::VerifiedOperator)
    }
}

/// The class of physical effect a capability may authorise.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum EffectClass {
    /// A credential-bearing request to a model provider.
    ProviderSend,
    /// An input event applied to a Computer Use surface.
    ComputerUseAct,
    /// Work handed to an external worker process.
    ExternalWorkerDispatch,
    /// An operator disposition of one already-recorded provider attempt.
    ///
    /// This never authorises a physical send. `begin_send` rejects it.
    OperatorReconcile,
}

impl EffectClass {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ProviderSend => "provider_send",
            Self::ComputerUseAct => "computer_use_act",
            Self::ExternalWorkerDispatch => "external_worker_dispatch",
            Self::OperatorReconcile => "operator_reconcile",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "provider_send" => Some(Self::ProviderSend),
            "computer_use_act" => Some(Self::ComputerUseAct),
            "external_worker_dispatch" => Some(Self::ExternalWorkerDispatch),
            "operator_reconcile" => Some(Self::OperatorReconcile),
            _ => None,
        }
    }
}

/// Explicit operator disposition of an uncertain (or still in-flight) attempt.
///
/// None of these variants performs provider I/O or resends work.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ReconciliationDisposition {
    /// Inspect the bound attempt at this revision. No state change.
    Review,
    /// Assert the attempt never reached the wire. Requires host pre-wire evidence.
    MarkNotSent,
    /// Assert the attempt took effect. Requires a provider receipt or independent
    /// operator observation digest; `observed_at` alone is not identity proof.
    MarkSettled,
    /// Explicitly discard the attempt without asserting provider effect.
    Discard,
}

impl ReconciliationDisposition {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::MarkNotSent => "mark_not_sent",
            Self::MarkSettled => "mark_settled",
            Self::Discard => "discard",
        }
    }

    pub(crate) fn truth(self) -> Option<&'static str> {
        match self {
            Self::Review => None,
            Self::MarkNotSent => Some("no_effect"),
            Self::MarkSettled => Some("took_effect"),
            Self::Discard => Some("discarded"),
        }
    }
}

/// Evidence the operator presents when settling or discarding an attempt.
///
/// Identity is a provider-receipt digest or an independent operator observation
/// digest. A wall-clock `observed_at_ms` is never sufficient on its own.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ReconciliationEvidence {
    pub(crate) provider_receipt: Option<ContentDigest>,
    pub(crate) operator_observation: Option<ContentDigest>,
    pub(crate) observed_at_ms: Option<u64>,
}

impl ReconciliationEvidence {
    /// Evidence that identifies a provider-issued receipt.
    pub fn provider_receipt(digest: ContentDigest) -> Self {
        Self {
            provider_receipt: Some(digest),
            operator_observation: None,
            observed_at_ms: None,
        }
    }

    /// Independent operator observation of provider truth. Not a timestamp.
    pub fn operator_observation(digest: ContentDigest) -> Self {
        Self {
            provider_receipt: None,
            operator_observation: Some(digest),
            observed_at_ms: None,
        }
    }

    /// Timestamp-only claim. This is never identity proof.
    pub fn observed_at_only(observed_at_ms: u64) -> Self {
        Self {
            provider_receipt: None,
            operator_observation: None,
            observed_at_ms: Some(observed_at_ms),
        }
    }

    /// Whether this evidence identifies a provider receipt or operator observation.
    pub fn has_identity_proof(&self) -> bool {
        self.provider_receipt.is_some() || self.operator_observation.is_some()
    }
}

/// Short-lived, one-use operator grant bound to one attempt at one revision.
///
/// Fields are private and there is no public constructor. The only producer is
/// [`crate::HostAuthority::mint_reconciliation_grant`].
#[must_use = "a reconciliation grant authorises nothing until it is spent"]
pub struct ReconciliationGrant {
    pub(crate) lease: EffectLease,
    pub(crate) attempt: AttemptId,
    pub(crate) revision: ContentDigest,
    pub(crate) state: String,
    pub(crate) dialect: String,
    pub(crate) route_digest: ContentDigest,
    pub(crate) disposition: ReconciliationDisposition,
    pub(crate) expires_at_ms: u64,
}

impl ReconciliationGrant {
    pub fn attempt(&self) -> AttemptId {
        self.attempt
    }
    pub fn revision(&self) -> ContentDigest {
        self.revision
    }
    pub fn disposition(&self) -> ReconciliationDisposition {
        self.disposition
    }
    pub fn expires_at_ms(&self) -> u64 {
        self.expires_at_ms
    }
    /// Effect class this grant authorises. Always operator reconcile, never send.
    pub fn effect(&self) -> EffectClass {
        self.lease.effect()
    }
}

impl std::fmt::Debug for ReconciliationGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReconciliationGrant")
            .field("attempt", &self.attempt.public_handle())
            .field("revision", &self.revision.public_handle())
            .field("disposition", &self.disposition)
            .finish_non_exhaustive()
    }
}

/// Gate 2 receipt: a one-use effect lease for exactly one action.
///
/// Bound to the action digest *and* the observation revision and digest the
/// action was planned against, so a lease cannot be replayed after the surface
/// has moved.
#[must_use = "an unspent lease authorises nothing; spend it or let it expire"]
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EffectLease {
    pub(crate) id: EffectLeaseId,
    pub(crate) capability: CapabilityId,
    pub(crate) binding: AuthorityBinding,
    pub(crate) observation_revision: ObservationRevision,
    pub(crate) observation_digest: ContentDigest,
    pub(crate) action_digest: ContentDigest,
    pub(crate) actor: ActorClass,
    pub(crate) effect: EffectClass,
    pub(crate) expires_at_ms: u64,
}

impl EffectLease {
    pub fn id(&self) -> EffectLeaseId {
        self.id
    }
    pub fn binding(&self) -> &AuthorityBinding {
        &self.binding
    }
    pub fn action_digest(&self) -> ContentDigest {
        self.action_digest
    }
    pub fn observation_revision(&self) -> ObservationRevision {
        self.observation_revision
    }
    pub fn effect(&self) -> EffectClass {
        self.effect
    }
    /// Who stands behind the capability this lease came from.
    pub fn actor(&self) -> ActorClass {
        self.actor
    }
}

/// Gate 3 receipt: the only thing that permits a physical provider send.
///
/// A permit is issued after the attempt and `SendIntent` are durable in
/// `Preparing`; [`HostAuthority::admit_sending`](crate::HostAuthority::admit_sending)
/// transitions it to `Sending` immediately before bytes may move. It is
/// consumed by value at settlement, so the type system enforces one-use: a
/// spent permit no longer exists to be presented again.
#[must_use = "holding a permit without settling it leaves the attempt Uncertain"]
#[derive(PartialEq, Eq)]
pub struct PhysicalSendPermit {
    pub(crate) attempt: AttemptId,
    pub(crate) lease: EffectLeaseId,
    pub(crate) binding: AuthorityBinding,
    pub(crate) request_digest: ContentDigest,
    pub(crate) body_digest: ContentDigest,
    pub(crate) idempotency_key: String,
    pub(crate) dialect: String,
    pub(crate) wire_admitted: bool,
}

impl PhysicalSendPermit {
    pub fn attempt(&self) -> AttemptId {
        self.attempt
    }
    pub fn binding(&self) -> &AuthorityBinding {
        &self.binding
    }
    /// The digest the permit is bound to: URL, method, dialect, credential,
    /// model, and body together.
    pub fn request_digest(&self) -> ContentDigest {
        self.request_digest
    }
    /// Idempotency key to place on the outbound request, when the provider
    /// supports one. Derived from the attempt, so a replay of the same attempt
    /// is deduplicated provider-side.
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Wire dialect class for this attempt.
    pub fn dialect(&self) -> &str {
        &self.dialect
    }

    /// Whether wire admission has durably transitioned this attempt to
    /// `sending` immediately before bytes may move.
    pub fn wire_admitted(&self) -> bool {
        self.wire_admitted
    }
}

impl std::fmt::Debug for PhysicalSendPermit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhysicalSendPermit")
            .field("attempt", &self.attempt.public_handle())
            .field("request_digest", &self.request_digest.public_handle())
            .finish_non_exhaustive()
    }
}

/// How a provider attempt ended.
///
/// There is no `retry` constructor and no way to turn [`Self::Uncertain`] back
/// into a fresh permit: an ambiguous attempt is settled by explicit host
/// reconciliation against the provider, never by automatically sending again.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SendOutcome {
    /// The provider accepted the request and the response was observed.
    Settled { attempt: AttemptId },
    /// Proven *not* to have reached the provider.
    ///
    /// Only reachable on paths where the store established that no byte of the
    /// request was written — a pre-dispatch denial, or a connect-time refusal.
    Failed {
        attempt: AttemptId,
        reason: FailedReason,
    },
    /// The request may or may not have taken physical effect.
    ///
    /// Every ambiguous path lands here: a transport error after the request
    /// began, a crash between dispatch and settlement, or trouble persisting
    /// the audit record once dispatch was already possible.
    Uncertain {
        attempt: AttemptId,
        reason: UncertainReason,
    },
}

impl SendOutcome {
    /// Whether a physical effect may have occurred.
    pub const fn may_have_taken_effect(&self) -> bool {
        matches!(self, Self::Settled { .. } | Self::Uncertain { .. })
    }

    /// Whether it is safe to send the same request again without operator
    /// reconciliation. Only ever true for a proven-no-effect failure.
    pub const fn is_safe_to_resend(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// Why an attempt provably never reached the provider.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FailedReason {
    /// Authority was refused before the request was constructed.
    DeniedBeforeDispatch,
    /// The connection was refused before any request byte was written.
    ConnectRefusedBeforeWrite,
    /// The attempt never reached the wire: preparing was durable but wire
    /// admission did not occur before abandonment or recovery.
    AbandonedBeforeWireAdmission,
}

/// Why an attempt is ambiguous.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum UncertainReason {
    /// The transport failed after the request may have been written.
    TransportAfterPossibleWrite,
    /// The caller cancelled after the request may have been written.
    CancelledAfterPossibleWrite,
    /// Response headers arrived, but the response body failed, was truncated,
    /// or was abandoned before the caller could validate the provider result.
    ResponseBodyAfterPossibleEffect,
    /// The complete response bytes were observed, but the provider protocol
    /// could not be validated. The request may still have taken effect.
    ProtocolAfterPossibleEffect,
    /// The process stopped between dispatch and settlement; recovery found the
    /// attempt still in flight.
    CrashBetweenDispatchAndSettlement,
    /// The audit record could not be persisted once a physical effect was
    /// already possible.
    ///
    /// This is the variant that keeps audit trouble from being reported as an
    /// ordinary failure: the effect may have happened, so the outcome stays
    /// ambiguous even though the local error was "just" a write error.
    AuditNotDurableAfterDispatch,
    /// The in-process lifecycle transaction could not be entered after a
    /// physical effect was already possible. This normally means another
    /// thread panicked while holding the lifecycle lock. No audit append was
    /// attempted, so this stays distinct from audit-media failure while still
    /// requiring operator/provider reconciliation.
    LifecycleUnavailableAfterDispatch,
    /// The outcome audit record is durable, but the derived state snapshot
    /// could not be updated. Open-time WAL replay will converge the snapshot;
    /// until then the caller must treat the local view as ambiguous.
    StateNotDurableAfterDispatch,
}
