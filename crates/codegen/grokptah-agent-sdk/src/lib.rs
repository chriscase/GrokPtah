//! # GrokPtah agent SDK
//!
//! A versioned, provider-neutral capability boundary for embedding the
//! GrokPtah agentic harness in another project.
//!
//! This crate is **contract only**. It depends on nothing from
//! `grokptah-agent-bridge`, holds no runtime state, opens no sockets, and
//! touches no filesystem. A consumer that imports it gets types and traits;
//! it does not get, and cannot reach, GrokPtah's internals.
//!
//! ## Why a separate crate
//!
//! `grokptah-agent-bridge` is the runtime: sessions, policy, durable stores,
//! provider profiles, keychain access, an HTTP control plane. Depending on it
//! to *talk to* an agent host would drag every one of those into a consumer's
//! dependency graph and would make each of its internal types part of that
//! consumer's compatibility surface. This crate is the narrow waist instead.
//!
//! ```text
//!   ContextDesk / another UI
//!            │  depends on
//!            ▼
//!   grokptah-agent-sdk   ← traits + DTOs + errors + capability document
//!            ▲  implemented by
//!            │
//!   ┌────────┴─────────┬───────────────────────┬──────────────────┐
//!   │ desktop adapter  │ ServiceControlPlane   │ FakeControlPlane │
//!   │ (in-process, P1) │ (ptah_* control plane)│ (deterministic)  │
//!   └──────────────────┴───────────────────────┴──────────────────┘
//!            │ calls
//!            ▼
//!   grokptah-agent-bridge (runtime; never a consumer dependency)
//! ```
//!
//! ## Getting started
//!
//! ```
//! # use grokptah_agent_sdk::prelude::*;
//! # async fn demo() -> SdkResult<()> {
//! let plane = FakeControlPlane::builder().build();
//! let connected = plane.connect().await?;
//! connected.require(&CapabilityId::TaskSubmit)?;
//!
//! let session = plane.seeded_session().expect("builder seeds one session");
//! let accepted = plane
//!     .submit_task(TaskSubmission {
//!         request_id: RequestId::new("req-0001")?,
//!         session_id: session.session_id.clone(),
//!         workspace: session.workspace.clone(),
//!         prompt: "summarize the build failures".into(),
//!         bounds: None,
//!         execution_mode: ExecutionMode::Shared,
//!         allow_queue: false,
//!     })
//!     .await?;
//! assert_eq!(accepted.lifecycle, RunLifecycle::Queued);
//! # Ok(())
//! # }
//! ```
//!
//! ## Boundaries this crate keeps
//!
//! * **Public projection vs. operator data.** Everything here is the public
//!   projection. Operator-only surfaces — permission prompts, credential
//!   management, provider profile editing, Computer Use grants, promotion of
//!   reviewed code — are absent by type. See [`dto`] for the full table.
//! * **Authority-owned secrets never cross.** Provider credentials and auth
//!   material are a permanently forbidden capability. The one secret that does
//!   exist on this boundary, a lease token, lives in [`dto::LeaseCredential`],
//!   which is not `Serialize`.
//! * **One lifecycle machine.** [`dto::RunLifecycle`] mirrors the runtime's
//!   run states exactly. This crate defines no second state machine.
//!
//! ## Status
//!
//! Pre-1.0 and not published. ADR-002 §7 gates SDK publication on a named
//! compatibility owner maintaining the parity matrix for a real external
//! consumer. The matrix itself lives in [`conformance`]. See
//! `docs/AGENT_SDK_SEAM.md` for the adapter mapping and the residual work.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]
#![warn(clippy::all)]

pub mod capability;
pub mod client;
pub mod dto;
pub mod error;
pub mod ids;
pub mod observe;
pub mod page;
pub mod service;
pub mod version;

#[cfg(feature = "conformance")]
pub mod conformance;

#[cfg(feature = "fake")]
pub mod fake;

/// Everything a consumer usually wants in scope.
pub mod prelude {
    pub use crate::capability::{
        Availability, BoundaryLimits, CapabilityDescriptor, CapabilityDocument, CapabilityId,
        HostDescriptor, HostKind,
    };
    pub use crate::client::{
        recover_mutation, AgentControlPlane, AgentControlPlaneExt, Connected, MutationRecovery,
    };
    pub use crate::dto::{
        AppliedBounds, ArtifactDescriptor, ArtifactKind, ArtifactMedia, ArtifactPayload,
        ArtifactRequest, BoundedText, CancelReceipt, CancelRequest, ChangedFile, ContentDigest,
        ControlLease, ControlLeaseRequest, CreateSessionRequest, ExecutionMode,
        FollowUpDisposition, FollowUpReceipt, FollowUpRequest, LeaseCredential, ObservationCounts,
        OperationClass, PublicEvent, PublicEventKind, ReceiptStatus, ReceiptView,
        ReleaseLeaseReceipt, ReleaseLeaseRequest, Revision, RevisionWatermark, RunAccepted,
        RunBoundsRequest, RunLifecycle, RunProgressView, RunSelector, RunView, SessionKind,
        SessionView, StopCause, TaskSubmission, ToolKind, ToolStatus, UsageView,
        VerificationStatus, VerificationView,
    };
    pub use crate::error::{ErrorOrigin, RetryDisposition, SdkError, SdkErrorCode, SdkResult};
    pub use crate::ids::{
        AgentId, ArtifactId, AttemptId, Label, RelativePath, RequestId, RunId, SessionId, WorkId,
        WorkspaceRef,
    };
    pub use crate::observe::{ObserverHandle, RunObservatory};
    pub use crate::page::{Cursor, Page, PageRequest, RetainedRange};
    pub use crate::service::{
        McpTransport, MutationAuthority, ServiceControlPlane, ServiceHostInfo, TransportFault,
        WorkspaceRegistry, TEST_REPORT_ARTIFACT_ID,
    };
    pub use crate::version::{negotiate, ContractVersion, Negotiated, CONTRACT_VERSION};

    #[cfg(feature = "fake")]
    pub use crate::fake::{FakeBuilder, FakeControlPlane, Fault, Operation, ScriptedOutcome};
}
