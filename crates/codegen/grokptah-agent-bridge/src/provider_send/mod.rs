//! The one physical provider-send authority (#478).
//!
//! # Why this module exists
//!
//! Instrumenting the *transport* is not enough. A previous design bound
//! attempts with a `tokio::task_local!`, which propagates across `.await` but
//! not across `tokio::spawn`; the result was a correct transport reached by
//! call sites that silently carried no binding, so their retry loops stayed
//! live and no attempt was ever recorded. Worse, two call sites — the provider
//! qualification probes — built their own `reqwest` client and never reached
//! the instrumented transport at all, while spending the operator's credential
//! on real model invocations.
//!
//! So the binding here is **not ambient**. [`ProviderSendContext`] is an
//! ordinary value threaded through call signatures, and the physical send path
//! takes an [`AttemptHandle`] it can only get from the ledger. "Unbound call
//! site" is not a lint: it does not compile.
//!
//! # The chokepoint
//!
//! [`transport`] is the only place in this crate that constructs an inference
//! client or a `/chat/completions` URL. `tests/provider_send_gate.rs` is the
//! structural gate that keeps it that way — it keys on *constructing a provider
//! request*, not on calling a particular helper, because that is exactly the
//! check the qualification probes would otherwise pass.
//!
//! # The lattice
//!
//! ```text
//!                      ┌─ host evidence ─────────────► NotSent
//!  Preparing ──────────┤
//!    (durable before   └─ pre-dispatch ─► Sending
//!     admission)                            │  (durable before any byte moves)
//!                                           ├─ connection never established ─► NotSent
//!                                           ├─ response headers ─► Acknowledged ─► Responding ─► Settled
//!                                           └─ anything else ────► Uncertain
//! ```
//!
//! `Uncertain` is not terminal and never auto-retries; it blocks the scope's
//! ordinal sequence until an explicit #466 grant resolves it.

pub mod crash;
pub mod dialect;
pub mod identity;
pub mod ledger;
pub mod projection;
pub mod record;
pub mod seams;
pub mod state;
pub mod transport;

use std::sync::Arc;

pub use crash::{
    arm_crash_cut, crash_cut_test_lock, disarm_crash_cut, CrashCut, CutAction, CutFired,
};
pub use dialect::{DialectIdempotency, ReceiptSource, WireDialect};
pub use identity::{
    AttemptBinding, AttemptBindingSpec, CallSiteFamily, HostIdempotencyIdentity, IdentityError,
    OpaqueId, RequestDigest, RouteIncarnation, SendOrigin, SendScope, ATTEMPT_BINDING_VERSION,
    HOST_IDEMPOTENCY_VERSION,
};
pub use ledger::{
    AttemptHandle, AttemptLedger, LedgerError, RecoveryReport, TakeoverOutcome, MAX_ORDINAL,
};
pub use projection::{
    ProviderAttemptProjection, SettlementProjection, PROVIDER_ATTEMPT_PROJECTION_VERSION,
};
pub use record::{
    AccountingRecord, AuditOutcome, CancellationRecord, HostIncarnationId, ProviderAttempt,
    ReceiptRecord, Settlement, SettlementContradiction, SettlementOutcome,
    PROVIDER_ATTEMPT_SCHEMA_VERSION,
};
pub use seams::{
    AuditGeneration, CapabilityGeneration, LifecycleGeneration, PrincipalGeneration,
    QueueOwnershipGeneration, ReconciliationGrant, ReconciliationResolution, SeamProvenance,
};
pub use state::{
    DeliveryKnowledge, HostEvidence, HostFailureClass, ProviderAttemptState, TransportEvidence,
    UncertaintyClass,
};
pub use transport::{
    dispatch, ProviderRequestSpec, ProviderSendError, ResponseAccept, ResponseReader, SentRequest,
    TransportOutcome,
};

/// In-memory scope inputs. Kept out of every durable record — only their
/// digests are stored — but retained here so one context can be re-scoped to a
/// nested call-site family without re-plumbing identifiers through the caller.
#[derive(Clone, Debug)]
struct ScopeInputs {
    workspace: String,
    session: String,
    run: Option<String>,
    origin: SendOrigin,
}

/// Everything a caller needs to open a bound attempt.
///
/// Cloneable and `Send + Sync`, so it crosses `tokio::spawn` the way an ambient
/// task-local could not. That is the whole point: a spawned subagent inherits
/// its binding because it is *handed* one, not because it happens to run on the
/// right task.
#[derive(Clone, Debug)]
pub struct ProviderSendContext {
    ledger: Arc<AttemptLedger>,
    inputs: ScopeInputs,
    family: CallSiteFamily,
    principal: PrincipalGeneration,
    capability: CapabilityGeneration,
    lifecycle: LifecycleGeneration,
    queue: QueueOwnershipGeneration,
    audit: AuditGeneration,
}

/// The authority generations bound to every attempt opened from a context.
#[derive(Clone, Debug)]
pub struct SendAuthorities {
    pub principal: PrincipalGeneration,
    pub capability: CapabilityGeneration,
    pub lifecycle: LifecycleGeneration,
    pub queue: QueueOwnershipGeneration,
    pub audit: AuditGeneration,
}

impl SendAuthorities {
    /// Derive stable provisional generations for the authorities that are not
    /// on the mainline yet (#477, #458, #455/#468, #461, #462).
    ///
    /// Inputs must be non-secret identifiers; they are hashed either way. When
    /// an authority lands, only this constructor is replaced.
    pub fn provisional(provider_id: &str, account: &str, policy: &str) -> Self {
        Self {
            principal: PrincipalGeneration::provisional(&[provider_id, account]),
            capability: CapabilityGeneration::provisional(&[policy]),
            lifecycle: LifecycleGeneration::provisional(&[provider_id, account]),
            queue: QueueOwnershipGeneration::provisional(&[provider_id]),
            audit: AuditGeneration::provisional(&[provider_id, account]),
        }
    }
}

impl ProviderSendContext {
    pub fn new(
        ledger: Arc<AttemptLedger>,
        workspace: &str,
        session: &str,
        run: Option<&str>,
        origin: SendOrigin,
        family: CallSiteFamily,
        authorities: SendAuthorities,
    ) -> Result<Self, IdentityError> {
        // Validate the scope eagerly so a bad scope fails at context creation
        // rather than at the first send.
        let _ = SendScope::new(workspace, session, run, origin, family)?;
        Ok(Self {
            ledger,
            inputs: ScopeInputs {
                workspace: workspace.to_string(),
                session: session.to_string(),
                run: run.filter(|value| !value.is_empty()).map(str::to_string),
                origin,
            },
            family,
            principal: authorities.principal,
            capability: authorities.capability,
            lifecycle: authorities.lifecycle,
            queue: authorities.queue,
            audit: authorities.audit,
        })
    }

    /// Build a context rooted at an explicit ledger directory.
    ///
    /// For harnesses that need a real binding without a running host: the
    /// conformance replay, the crash-cut helper binary, and unit tests. It is a
    /// genuine ledger under `root`, not a stub, so a test cannot accidentally
    /// exercise a weaker path than production.
    pub fn for_root(
        root: impl Into<std::path::PathBuf>,
        workspace: &str,
        session: &str,
        origin: SendOrigin,
        family: CallSiteFamily,
    ) -> Result<Self, LedgerError> {
        let ledger = Arc::new(AttemptLedger::open(root)?);
        Self::new(
            ledger,
            workspace,
            session,
            None,
            origin,
            family,
            SendAuthorities::provisional("harness", session, "harness"),
        )
        .map_err(|_| LedgerError::Io(std::io::Error::other("invalid send scope")))
    }

    /// Re-scope to a different physical send site within the same session.
    ///
    /// A subagent gets its own ordinal sequence — its sends are not the parent
    /// turn's sends — while keeping the same principal, capability, and run.
    pub fn for_family(&self, family: CallSiteFamily) -> Self {
        Self {
            family,
            ..self.clone()
        }
    }

    /// Re-scope to a different origin, for the same reason.
    pub fn for_origin(&self, origin: SendOrigin) -> Self {
        let mut next = self.clone();
        next.inputs.origin = origin;
        next
    }

    /// Attach or replace the run identifier.
    pub fn with_run(&self, run: Option<&str>) -> Self {
        let mut next = self.clone();
        next.inputs.run = run.filter(|value| !value.is_empty()).map(str::to_string);
        next
    }

    pub fn family(&self) -> CallSiteFamily {
        self.family
    }

    pub fn origin(&self) -> SendOrigin {
        self.inputs.origin
    }

    pub fn ledger(&self) -> &Arc<AttemptLedger> {
        &self.ledger
    }

    /// The durable scope this context's attempts are ordered within.
    pub fn scope(&self) -> SendScope {
        SendScope::new(
            &self.inputs.workspace,
            &self.inputs.session,
            self.inputs.run.as_deref(),
            self.inputs.origin,
            self.family,
        )
        .expect("scope inputs validated at construction")
    }

    /// Build the pre-ordinal half of a binding for one concrete request.
    pub fn binding_spec(
        &self,
        route: RouteIncarnation,
        request_digest: RequestDigest,
    ) -> AttemptBindingSpec {
        AttemptBindingSpec {
            scope: self.scope(),
            principal: self.principal.clone(),
            capability: self.capability.clone(),
            lifecycle: self.lifecycle.clone(),
            queue: self.queue.clone(),
            audit: self.audit.clone(),
            route,
            request_digest,
        }
    }

    /// Persist `Preparing` and admit one attempt.
    pub fn begin_attempt(
        &self,
        route: RouteIncarnation,
        request_digest: RequestDigest,
    ) -> Result<AttemptHandle, LedgerError> {
        self.ledger
            .begin_attempt(self.binding_spec(route, request_digest))
    }

    /// Reconstruct this context's scope after a restart.
    pub fn recover(&self) -> Result<RecoveryReport, LedgerError> {
        self.ledger.recover_scope(&self.scope())
    }

    /// Public, redacted view of every attempt in this context's scope.
    pub fn projections(&self) -> Result<Vec<ProviderAttemptProjection>, LedgerError> {
        Ok(self
            .ledger
            .list_scope(&self.scope())?
            .iter()
            .map(ProviderAttemptProjection::of)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(dir: &tempfile::TempDir, family: CallSiteFamily) -> ProviderSendContext {
        ProviderSendContext::new(
            Arc::new(AttemptLedger::open(dir.path()).expect("ledger")),
            "/workspace",
            "session-1",
            Some("run-1"),
            SendOrigin::Desktop,
            family,
            SendAuthorities::provisional("provider", "account", "policy"),
        )
        .expect("context")
    }

    fn route() -> RouteIncarnation {
        RouteIncarnation::new(
            "https://gateway.invalid",
            "model",
            WireDialect::OpenAiChatCompletions,
            "gateway_api_key",
            None,
        )
    }

    #[test]
    fn a_context_crosses_a_spawn_boundary() {
        let dir = tempfile::tempdir().expect("tmp");
        let context = context(&dir, CallSiteFamily::DesktopBuildRound);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let moved = context.clone();
        let ordinal = runtime.block_on(async move {
            tokio::spawn(async move {
                let handle = moved
                    .begin_attempt(route(), RequestDigest::of_body(b"body"))
                    .expect("bound inside a spawned task");
                handle.ordinal()
            })
            .await
            .expect("join")
        });
        assert_eq!(ordinal, 1);
    }

    #[test]
    fn nested_families_get_independent_ordinal_sequences() {
        let dir = tempfile::tempdir().expect("tmp");
        let parent = context(&dir, CallSiteFamily::DesktopBuildRound);
        let child = parent.for_family(CallSiteFamily::ExploreSubagent);

        let parent_handle = parent
            .begin_attempt(route(), RequestDigest::of_body(b"a"))
            .expect("parent");
        let child_handle = child
            .begin_attempt(route(), RequestDigest::of_body(b"b"))
            .expect("child");

        assert_eq!(parent_handle.ordinal(), 1);
        assert_eq!(child_handle.ordinal(), 1);
        assert_ne!(
            parent_handle.binding().host_idempotency().key(),
            child_handle.binding().host_idempotency().key()
        );
    }

    #[test]
    fn projections_of_a_scope_are_redacted() {
        let dir = tempfile::tempdir().expect("tmp");
        let context = context(&dir, CallSiteFamily::PlanProposal);
        let _handle = context
            .begin_attempt(route(), RequestDigest::of_body(b"prompt"))
            .expect("begin");
        let projections = context.projections().expect("projections");
        assert_eq!(projections.len(), 1);
        let json = serde_json::to_string(&projections).expect("serialize");
        assert!(!json.contains("/workspace"));
        assert!(!json.contains("session-1"));
        assert!(!json.contains("gateway.invalid"));
    }
}
