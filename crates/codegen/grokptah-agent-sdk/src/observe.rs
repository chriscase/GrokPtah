//! The read-only half of the boundary.
//!
//! [`AgentControlPlane`] mixes reads and mutations because a host adapter
//! needs both. An external consumer usually needs only the reads — and
//! "usually" is not a security property. This module makes it one.
//!
//! [`RunObservatory`] contains the read operations and nothing else, and
//! [`ObserverHandle`] wraps any control plane so that only those operations
//! remain reachable. The wrapper deliberately does **not** implement
//! [`AgentControlPlane`], so a consumer holding one cannot submit, cancel,
//! steer, create, or lease — not because a policy check refuses, but because
//! the methods are not there. There is no flag to flip and no downcast to
//! find.
//!
//! This is stricter than [`ServiceControlPlane::read_only`], which enforces
//! the same restriction at call time on a value that still *has* the mutating
//! methods. Both are useful: the adapter's mode protects an embedder that
//! keeps one plane for everything, and the handle is what you pass across a
//! trust boundary.
//!
//! [`ServiceControlPlane::read_only`]: crate::service::ServiceControlPlane::read_only

use async_trait::async_trait;

use crate::capability::{Availability, CapabilityDescriptor, CapabilityDocument};
use crate::client::AgentControlPlane;
use crate::dto::{
    ArtifactPayload, ArtifactRequest, PublicEvent, ReceiptPage, RunSelector, RunView, SessionView,
};
use crate::error::SdkResult;
use crate::ids::Label;
use crate::page::{Page, PageRequest};

/// Everything a consumer may do without holding authority.
///
/// Every method is a read. There is no mutating method to omit, forget, or
/// guard — the shape of the trait is the guarantee.
#[async_trait]
pub trait RunObservatory: Send + Sync {
    /// What this host offers, with every mutating capability reported as
    /// [`Availability::Forbidden`]. An observatory never advertises authority
    /// it cannot exercise.
    async fn capabilities(&self) -> SdkResult<CapabilityDocument>;

    async fn list_sessions(&self, page: PageRequest) -> SdkResult<Page<SessionView>>;

    async fn observe_run(&self, selector: RunSelector) -> SdkResult<RunView>;

    async fn stream_events(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<Page<PublicEvent>>;

    async fn fetch_artifact(&self, request: ArtifactRequest) -> SdkResult<ArtifactPayload>;

    /// Redacted receipts for mutations attributed to one run.
    ///
    /// Run-scoped by construction: there is no global receipt listing, for the
    /// same reason there is no global event dump. A mutation with no run — a
    /// session creation, a lease — is not listed here.
    async fn list_receipts(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<ReceiptPage>;
}

/// A control plane narrowed to its reads.
///
/// Construct one and hand it out; the holder gets [`RunObservatory`] and
/// nothing else. Wrapping is one-way on purpose — there is no accessor that
/// returns the inner plane, because an accessor would be the escape hatch this
/// type exists to remove.
#[derive(Debug, Clone)]
pub struct ObserverHandle<T> {
    inner: T,
}

impl<T: AgentControlPlane> ObserverHandle<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

/// Narrow a capability document to what an observer may actually use.
///
/// Availability is only ever *reduced*: an already-`Unsupported` capability
/// keeps that answer, because "this host cannot" is more informative than
/// "you may not" and stays true either way.
fn narrow(document: CapabilityDocument) -> CapabilityDocument {
    let reason = Label::new("held through a read-only observer handle").expect("static label");
    let host_version = document.contract_version;
    let narrowed: Vec<CapabilityDescriptor> = document
        .iter()
        .map(|descriptor| {
            if descriptor.id.is_mutation()
                && matches!(descriptor.availability, Availability::Available)
            {
                CapabilityDescriptor {
                    availability: Availability::Forbidden {
                        reason: reason.clone(),
                    },
                    ..descriptor.clone()
                }
            } else {
                descriptor.clone()
            }
        })
        .collect();
    let mut document = CapabilityDocument::new(
        document.host.clone(),
        document.asserted_at,
        document.limits,
        narrowed,
    );
    // `CapabilityDocument::new` stamps this build's contract version; the
    // host's answer is what a consumer negotiates against, so carry it through
    // rather than silently upgrading it.
    document.contract_version = host_version;
    document
}

#[async_trait]
impl<T: AgentControlPlane> RunObservatory for ObserverHandle<T> {
    async fn capabilities(&self) -> SdkResult<CapabilityDocument> {
        Ok(narrow(self.inner.capabilities().await?))
    }

    async fn list_sessions(&self, page: PageRequest) -> SdkResult<Page<SessionView>> {
        self.inner.list_sessions(page).await
    }

    async fn observe_run(&self, selector: RunSelector) -> SdkResult<RunView> {
        self.inner.observe_run(selector).await
    }

    async fn stream_events(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<Page<PublicEvent>> {
        self.inner.stream_events(selector, page).await
    }

    async fn fetch_artifact(&self, request: ArtifactRequest) -> SdkResult<ArtifactPayload> {
        self.inner.fetch_artifact(request).await
    }

    async fn list_receipts(
        &self,
        selector: RunSelector,
        page: PageRequest,
    ) -> SdkResult<ReceiptPage> {
        self.inner.list_receipts(selector, page).await
    }
}

#[cfg(test)]
mod tests {
    use crate::capability::CapabilityId;

    #[test]
    fn every_capability_is_classified_and_unknown_ones_count_as_mutations() {
        for id in [
            CapabilityId::SessionList,
            CapabilityId::RunObserve,
            CapabilityId::RunEventsPage,
            CapabilityId::RunEventsLive,
            CapabilityId::ArtifactFetch,
            CapabilityId::ComputerRead,
            CapabilityId::ReceiptRead,
        ] {
            assert!(!id.is_mutation(), "{id} must be readable by an observer");
        }
        for id in [
            CapabilityId::SessionCreate,
            CapabilityId::TaskSubmit,
            CapabilityId::RunFollowUp,
            CapabilityId::RunCancel,
            CapabilityId::ControlLease,
            CapabilityId::ComputerControl,
            CapabilityId::ProviderCredentials,
        ] {
            assert!(id.is_mutation(), "{id} must be withheld from an observer");
        }
        // The one that matters: a capability a future host invents is withheld
        // until this build knows what it does.
        assert!(CapabilityId::Unknown("workload.federate".into()).is_mutation());
    }
}
