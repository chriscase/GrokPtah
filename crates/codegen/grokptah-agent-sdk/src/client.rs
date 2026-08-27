//! The capability boundary itself.
//!
//! One trait, one host. An adapter implements [`AgentControlPlane`] over
//! whatever transport it owns — an in-process desktop runtime, the
//! authenticated MCP control plane of `grokptah-service`, or a deterministic
//! fake. A consumer such as ContextDesk depends on the trait, never on a
//! concrete adapter, and therefore never on GrokPtah's internals.
//!
//! # Contract every adapter owes its caller
//!
//! 1. **Discovery precedes use.** [`AgentControlPlane::capabilities`] is
//!    callable before anything else and answers without side effects. Calling
//!    a method whose capability is not `Available` returns the typed denial
//!    from the document, never a partial effect.
//! 2. **Every mutation is idempotent by [`RequestId`].** Same key, same
//!    payload replays the original receipt with `replayed: true`. Same key,
//!    different payload is [`SdkErrorCode::Conflict`]. A mutation whose
//!    outcome the host cannot determine is [`SdkErrorCode::UncertainOutcome`]
//!    and must never be retried automatically.
//! 3. **Reads are scoped, never oracles.** A run read takes the full
//!    [`RunSelector`]. Unknown run, another session's run, and another
//!    workspace's run return the *identical* [`SdkErrorCode::ForbiddenScope`],
//!    so a read cannot be used to probe for existence outside the caller's
//!    scope.
//! 4. **Nothing widens.** An adapter may narrow what a host offers. It may
//!    never advertise, synthesize, or forward authority the host did not grant.
//! 5. **Artifacts are verified before return.** See
//!    [`ArtifactPayload::verify`].
//!
//! The [`conformance`](crate::conformance) battery checks all five against any
//! adapter, which is what makes "implements the trait" mean something.

use async_trait::async_trait;

use crate::capability::{CapabilityDocument, CapabilityId};
use crate::dto::{
    ArtifactPayload, ArtifactRequest, CancelReceipt, CancelRequest, ControlLease,
    ControlLeaseRequest, CreateSessionRequest, FollowUpReceipt, FollowUpRequest, PublicEvent,
    ReceiptPage, ReleaseLeaseReceipt, ReleaseLeaseRequest, RunAccepted, RunSelector, RunView,
    SessionView, TaskSubmission,
};
use crate::error::{SdkError, SdkErrorCode, SdkResult};
use crate::ids::RequestId;
use crate::page::{Page, PageRequest};
use crate::version::{negotiate, Negotiated, CONTRACT_VERSION};

/// The GrokPtah agent capability boundary.
///
/// `Send + Sync` because a UI holds one instance behind shared state and calls
/// it from many tasks. Adapters own their own connection pooling.
#[async_trait]
pub trait AgentControlPlane: Send + Sync {
    /// What this host offers this credential right now.
    async fn capabilities(&self) -> SdkResult<CapabilityDocument>;

    /// Create a Build session on an already-advertised workspace.
    ///
    /// Session creation never accepts an arbitrary path or model policy: the
    /// caller selects a workspace the host allowlisted, nothing more.
    async fn create_session(&self, request: CreateSessionRequest) -> SdkResult<SessionView>;

    /// List sessions this credential may address.
    async fn list_sessions(&self, page: PageRequest) -> SdkResult<Page<SessionView>>;

    /// Submit one finite task run.
    async fn submit_task(&self, request: TaskSubmission) -> SdkResult<RunAccepted>;

    /// Read one run's public projection.
    async fn observe_run(&self, selector: RunSelector) -> SdkResult<RunView>;

    /// Page a run's bounded public event journal.
    ///
    /// A cursor below the retained window is [`SdkErrorCode::CursorExpired`]
    /// carrying the still-readable range, so recovery needs no second call.
    async fn stream_events(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<Page<PublicEvent>>;

    /// Send a non-cancelling follow-up.
    async fn request_follow_up(&self, request: FollowUpRequest) -> SdkResult<FollowUpReceipt>;

    /// Cancel a run explicitly.
    async fn cancel_run(&self, request: CancelRequest) -> SdkResult<CancelReceipt>;

    /// Acquire a durable work lease.
    async fn acquire_control(&self, request: ControlLeaseRequest) -> SdkResult<ControlLease>;

    /// Release a lease held by this caller.
    async fn release_control(&self, request: ReleaseLeaseRequest)
        -> SdkResult<ReleaseLeaseReceipt>;

    /// Fetch one bounded, digest-verified artifact.
    async fn fetch_artifact(&self, request: ArtifactRequest) -> SdkResult<ArtifactPayload>;

    /// Redacted receipts for mutations attributed to one run.
    ///
    /// Additive at contract 1.1, with a default that fails closed: an adapter
    /// written against 1.0 keeps compiling and reports the capability as
    /// absent rather than returning an empty page, which a consumer would
    /// otherwise read as "this run has had no mutations".
    async fn list_receipts(
        &self,
        _selector: RunSelector,
        _page: PageRequest,
    ) -> SdkResult<ReceiptPage> {
        Err(SdkError::new(
            SdkErrorCode::CapabilityUnavailable,
            "this adapter does not serve redacted receipts",
        )
        .with_detail("capability", CapabilityId::ReceiptRead.as_wire()))
    }
}

/// Convenience layer over any [`AgentControlPlane`].
///
/// Everything here is derived from the trait; an adapter never implements it.
/// Keeping it separate means adding a helper is not a contract change.
#[async_trait]
pub trait AgentControlPlaneExt: AgentControlPlane {
    /// Fetch the capability document and negotiate contract versions in one
    /// step. Use this at connect time and keep the result for the session.
    async fn connect(&self) -> SdkResult<Connected> {
        let document = self.capabilities().await?;
        let negotiated = negotiate(CONTRACT_VERSION, document.contract_version)?;
        Ok(Connected {
            negotiated,
            document,
        })
    }
}

impl<T: AgentControlPlane + ?Sized> AgentControlPlaneExt for T {}

/// A negotiated connection: what both sides agreed on, and what is offered.
#[derive(Debug, Clone)]
pub struct Connected {
    pub negotiated: Negotiated,
    pub document: CapabilityDocument,
}

impl Connected {
    /// Gate a call on a capability *and* on the negotiated minor.
    ///
    /// A capability introduced above the effective minor is refused even when
    /// the host says it is available: the consumer was not compiled against
    /// its shape, so calling it would decode a field set it does not know.
    pub fn require(&self, id: &CapabilityId) -> SdkResult<()> {
        let descriptor = self.document.require(id)?;
        if descriptor.since.minor > self.negotiated.effective.minor {
            return Err(SdkError::new(
                SdkErrorCode::ContractVersionUnsupported,
                format!(
                    "capability {id} requires contract {} but this connection negotiated {}",
                    descriptor.since, self.negotiated.effective
                ),
            )
            .with_detail("capability", id.as_wire())
            .with_detail("requiredContractVersion", descriptor.since.to_string())
            .with_detail(
                "negotiatedContractVersion",
                self.negotiated.effective.to_string(),
            ));
        }
        Ok(())
    }

    /// `true` when the consumer is newer than the host and must keep
    /// above-effective features switched off.
    pub fn is_degraded(&self) -> bool {
        self.negotiated.degraded
    }
}

/// How a caller should react to a failed mutation.
///
/// This exists so retry policy is a *decision the SDK states*, not folklore
/// each consumer reimplements — and so the `Uncertain` case cannot be
/// accidentally collapsed into "retry".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MutationRecovery {
    /// Retry with the **same** [`RequestId`]. The host will replay if the
    /// original attempt actually landed.
    RetrySameKey(RequestId),
    /// Do not retry. Fix the request or give up.
    DoNotRetry,
    /// The mutation may have taken effect. Reconcile by reading current state
    /// before deciding anything; never retry automatically.
    ReconcileFirst,
}

/// Classify a mutation failure.
pub fn recover_mutation(request_id: &RequestId, error: &SdkError) -> MutationRecovery {
    use crate::error::RetryDisposition;
    match error.retry_disposition() {
        RetryDisposition::Safe => MutationRecovery::RetrySameKey(request_id.clone()),
        RetryDisposition::Unsafe => MutationRecovery::ReconcileFirst,
        RetryDisposition::Never => MutationRecovery::DoNotRetry,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{
        Availability, BoundaryLimits, CapabilityDescriptor, CapabilityDocument, HostDescriptor,
        HostKind,
    };
    use crate::ids::Label;
    use crate::version::ContractVersion;
    use chrono::{DateTime, Utc};

    fn document(since: ContractVersion) -> CapabilityDocument {
        CapabilityDocument::new(
            HostDescriptor {
                kind: HostKind::Fake,
                product: Label::new("GrokPtah").unwrap(),
                host_version: Label::new("test").unwrap(),
            },
            DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            BoundaryLimits::default(),
            [CapabilityDescriptor {
                id: CapabilityId::TaskSubmit,
                since,
                availability: Availability::Available,
            }],
        )
    }

    #[test]
    fn capability_above_the_negotiated_minor_is_refused() {
        let connected = Connected {
            negotiated: negotiate(ContractVersion::new(1, 0), ContractVersion::new(1, 0)).unwrap(),
            document: document(ContractVersion::new(1, 3)),
        };
        let err = connected.require(&CapabilityId::TaskSubmit).unwrap_err();
        assert_eq!(err.code, SdkErrorCode::ContractVersionUnsupported);
        assert_eq!(err.detail("requiredContractVersion"), Some("1.3"));
        assert_eq!(err.detail("negotiatedContractVersion"), Some("1.0"));
    }

    #[test]
    fn capability_at_or_below_the_negotiated_minor_is_allowed() {
        let connected = Connected {
            negotiated: negotiate(ContractVersion::new(1, 4), ContractVersion::new(1, 4)).unwrap(),
            document: document(ContractVersion::new(1, 2)),
        };
        assert!(connected.require(&CapabilityId::TaskSubmit).is_ok());
    }

    #[test]
    fn recovery_never_tells_a_caller_to_retry_an_uncertain_mutation() {
        let key = RequestId::new("req-1").unwrap();
        assert_eq!(
            recover_mutation(
                &key,
                &SdkError::new(SdkErrorCode::UncertainOutcome, "in flight")
            ),
            MutationRecovery::ReconcileFirst
        );
        assert_eq!(
            recover_mutation(&key, &SdkError::new(SdkErrorCode::Timeout, "slow")),
            MutationRecovery::RetrySameKey(key.clone())
        );
        assert_eq!(
            recover_mutation(&key, &SdkError::new(SdkErrorCode::ForbiddenScope, "nope")),
            MutationRecovery::DoNotRetry
        );
    }
}
