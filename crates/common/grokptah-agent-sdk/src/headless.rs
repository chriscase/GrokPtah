//! Host-neutral headless authority entry point.
//!
//! This module is the seam a Linux or cloud worker embeds to expose GrokPtah
//! authority without the desktop. It is deliberately *only* a port: it owns no
//! process, no filesystem, no credential store, no provider client, and no
//! platform integration, so it compiles anywhere the SDK compiles — no Tauri,
//! no macOS frameworks, no D-Bus, no system C library.
//!
//! # Authorization
//!
//! [`HeadlessAdmission`] is not a second authorization model. It admits work
//! against the *existing* [`CapabilitySet`] contract, using the capability
//! identifiers the trusted host already advertises, and it can only ever
//! narrow: it refuses anything the host did not advertise as available. An
//! embedder still applies the host's own policy after admission.
//!
//! # Fail-closed by construction
//!
//! Every [`HeadlessAuthority`] operation defaults to
//! [`ErrorCode::AuthorityUnavailable`]. An embedder that implements only part
//! of the port cannot accidentally expose an unimplemented operation as a
//! success, and a capability the host does not advertise is never admitted.

use crate::CONTRACT_VERSION;
use crate::capability::{CapabilityAvailability, CapabilitySet};
use crate::error::{ErrorCode, ErrorEnvelope};
use crate::projection::ensure_share_safe_metadata;
use crate::run::{
    AuthorityBounds, DurableRun, ReviewReceipt, RunEventPage, RunScope, SubmitTaskRequest,
};

/// Stable identifier for the headless embedding contract.
pub const HEADLESS_CONTRACT_VERSION: &str = "grokptah.headless.v1";

/// Maximum UTF-8 bytes in a headless host identity.
pub const MAX_HOST_ID_BYTES: usize = 128;

/// Where a headless authority is running.
///
/// This exists so a consumer can reason about platform-gated capabilities
/// without the SDK depending on any platform. It is descriptive only: it
/// grants nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadlessPlatform {
    /// A Linux container, VM, or bare-metal worker.
    Linux,
    /// A macOS host running without the desktop UI attached.
    MacOs,
    /// A Windows host running without the desktop UI attached.
    Windows,
    /// A platform the consumer must treat as capability-less.
    Unknown,
}

impl HeadlessPlatform {
    /// Whether native Computer Use can exist on this platform at all.
    ///
    /// This is a *necessary*, never a sufficient, condition: the host still
    /// decides availability, and a lease is still required.
    pub fn supports_native_computer_use(self) -> bool {
        matches!(self, Self::MacOs)
    }
}

/// A monotonic capability revision.
///
/// A consumer caches a negotiated [`CapabilitySet`] against a revision. When
/// the authority's revision advances, the cached set is stale and every
/// operation admitted against it must fail closed until re-negotiation.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(transparent)]
pub struct CapabilityRevision(pub u64);

impl CapabilityRevision {
    /// The revision a host starts at before advertising anything.
    pub const INITIAL: Self = Self(0);

    /// Next revision, saturating rather than wrapping back to a stale value.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Whether `self` is behind `authority`.
    pub fn is_stale_against(self, authority: Self) -> bool {
        self.0 != authority.0
    }
}

/// Share-safe description of a headless authority.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadlessHostInfo {
    /// Opaque, share-safe host identity. Never a hostname-derived path.
    pub host_id: String,
    /// Capability contract version. Must equal [`CONTRACT_VERSION`].
    pub contract: String,
    /// Headless embedding contract. Must equal [`HEADLESS_CONTRACT_VERSION`].
    pub headless_contract: String,
    /// Platform the worker runs on.
    pub platform: HeadlessPlatform,
    /// Revision of `capabilities`.
    pub revision: CapabilityRevision,
    /// Capabilities this worker advertises.
    pub capabilities: CapabilitySet,
}

impl HeadlessHostInfo {
    /// Validate the advertisement before a consumer enables any operation.
    ///
    /// Rejects a version mismatch on either contract, a host identity that is
    /// not share-safe, and a capability set that is not well-formed.
    pub fn validate(&self) -> Result<(), ErrorEnvelope> {
        ensure_share_safe_metadata("host_id", &self.host_id, MAX_HOST_ID_BYTES).map_err(
            |finding| {
                invalid(
                    finding.kind.reason_code(),
                    "host identity is not share-safe",
                )
            },
        )?;
        if self.contract != CONTRACT_VERSION {
            return Err(invalid(
                "capability_contract_mismatch",
                "capability contract version is not supported",
            ));
        }
        if self.headless_contract != HEADLESS_CONTRACT_VERSION {
            return Err(invalid(
                "headless_contract_mismatch",
                "headless contract version is not supported",
            ));
        }
        if !self.capabilities.is_current() {
            return Err(invalid(
                "capability_set_malformed",
                "advertised capability set is not well-formed",
            ));
        }
        Ok(())
    }
}

/// Bounded ceilings a headless worker enforces.
///
/// Carries no path, credential, endpoint, or provider identity, so it is safe
/// to publish alongside [`HeadlessHostInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadlessLimits {
    /// Per-run bounds ceiling at the authority's integer widths.
    pub bounds: AuthorityBounds,
    /// Maximum runs admitted concurrently.
    pub max_concurrent_runs: u32,
}

impl HeadlessLimits {
    /// Reject a ceiling that admits unbounded work.
    pub fn validate(&self) -> Result<(), ErrorEnvelope> {
        self.bounds
            .validate()
            .map_err(|error| invalid(error.reason_code(), "headless bounds ceiling is invalid"))?;
        if self.max_concurrent_runs == 0 {
            return Err(invalid(
                "concurrency_zero",
                "max_concurrent_runs must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// One operation a headless worker may be asked to perform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadlessOperation {
    /// Submit a bounded durable run.
    Submit,
    /// Read a bounded page of durable events.
    Events,
    /// Read the bounded review projection for an isolated run.
    Review,
    /// Cancel a run the caller owns.
    Cancel,
}

impl HeadlessOperation {
    /// The capability identifier this operation requires.
    ///
    /// These are the identifiers the trusted host already advertises; the
    /// headless port introduces none of its own, so it cannot widen the
    /// authority surface.
    pub fn capability_id(self) -> &'static str {
        match self {
            Self::Submit | Self::Cancel => "run.execute",
            Self::Events => "session.observe",
            Self::Review => "run.review",
        }
    }

    /// Whether the operation can change durable state.
    pub fn is_mutating(self) -> bool {
        matches!(self, Self::Submit | Self::Cancel)
    }
}

/// The port a Linux or cloud worker implements to serve GrokPtah authority.
///
/// Every operation defaults to [`ErrorCode::AuthorityUnavailable`], so an
/// embedder exposes only what it has actually implemented.
pub trait HeadlessAuthority {
    /// Share-safe advertisement for this worker.
    fn host_info(&self) -> Result<HeadlessHostInfo, ErrorEnvelope>;

    /// Bounded ceilings this worker enforces.
    fn limits(&self) -> Result<HeadlessLimits, ErrorEnvelope>;

    /// Submit a bounded durable run.
    fn submit(&self, request: &SubmitTaskRequest) -> Result<DurableRun, ErrorEnvelope> {
        let _ = request;
        Err(unsupported(HeadlessOperation::Submit))
    }

    /// Read durable events after `after_seq`.
    fn events(&self, scope: &RunScope, after_seq: u64) -> Result<RunEventPage, ErrorEnvelope> {
        let _ = (scope, after_seq);
        Err(unsupported(HeadlessOperation::Events))
    }

    /// Read the bounded review projection for an isolated run.
    fn review(&self, scope: &RunScope) -> Result<ReviewReceipt, ErrorEnvelope> {
        let _ = scope;
        Err(unsupported(HeadlessOperation::Review))
    }

    /// Cancel a run the caller owns.
    fn cancel(&self, scope: &RunScope) -> Result<(), ErrorEnvelope> {
        let _ = scope;
        Err(unsupported(HeadlessOperation::Cancel))
    }
}

/// Admission gate binding one consumer to one exact scope and revision.
///
/// The gate never grants authority. It refuses work that the host did not
/// advertise, that is bound to a different session or workspace, that was
/// approved against a stale capability revision, or whose bounds exceed the
/// worker's ceiling.
#[derive(Debug, Clone)]
pub struct HeadlessAdmission {
    host: HeadlessHostInfo,
    limits: HeadlessLimits,
    bound_session_id: String,
    bound_workspace: String,
}

impl HeadlessAdmission {
    /// Bind an admission gate to one session/workspace pair.
    ///
    /// Validates the advertisement and the ceilings up front so a malformed
    /// worker cannot admit anything at all.
    pub fn bind(
        host: HeadlessHostInfo,
        limits: HeadlessLimits,
        session_id: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Result<Self, ErrorEnvelope> {
        host.validate()?;
        limits.validate()?;
        let bound_session_id = session_id.into();
        let bound_workspace = workspace.into();
        if bound_session_id.trim().is_empty() || bound_workspace.trim().is_empty() {
            return Err(invalid(
                "scope_unbound",
                "admission requires an exact session and workspace",
            ));
        }
        Ok(Self {
            host,
            limits,
            bound_session_id,
            bound_workspace,
        })
    }

    /// The advertisement this gate admits against.
    pub fn host(&self) -> &HeadlessHostInfo {
        &self.host
    }

    /// The ceilings this gate enforces.
    pub fn limits(&self) -> HeadlessLimits {
        self.limits
    }

    /// Reject a capability revision the consumer negotiated earlier.
    pub fn ensure_revision(&self, observed: CapabilityRevision) -> Result<(), ErrorEnvelope> {
        if observed.is_stale_against(self.host.revision) {
            return Err(ErrorEnvelope {
                code: ErrorCode::StaleOrRecovery,
                message: "capability revision is stale; re-negotiate before retrying".into(),
                request_id: None,
                reason_code: Some("capability_revision_stale".into()),
                event_range: None,
            });
        }
        Ok(())
    }

    /// Admit a scoped, non-submitting operation.
    pub fn admit(
        &self,
        operation: HeadlessOperation,
        scope: &RunScope,
        observed: CapabilityRevision,
    ) -> Result<(), ErrorEnvelope> {
        self.ensure_revision(observed)?;
        scope
            .validate()
            .map_err(|reason| invalid(reason, "run scope is invalid"))?;
        self.ensure_scope_binding(&scope.session_id, &scope.workspace)?;
        self.ensure_capability(operation)
    }

    /// Admit a submit request and resolve its bounds at the authority widths.
    ///
    /// Returns the resolved ceiling-narrowed bounds so the embedder never has
    /// to cast between the public and authority integer widths itself.
    pub fn admit_submit(
        &self,
        request: &SubmitTaskRequest,
        observed: CapabilityRevision,
    ) -> Result<AuthorityBounds, ErrorEnvelope> {
        self.ensure_revision(observed)?;
        request
            .validate()
            .map_err(|reason| invalid(reason, "submit request is invalid"))?;
        self.ensure_scope_binding(&request.session_id, &request.workspace)?;
        self.ensure_capability(HeadlessOperation::Submit)?;

        let bounds = request.bounds.clone().unwrap_or_default();
        let resolved = bounds
            .resolve_authority_widths(self.limits.bounds)
            .map_err(|error| invalid(error.reason_code(), "bounds exceed the worker ceiling"))?;
        if request.prompt.len() > resolved.max_prompt_bytes {
            return Err(invalid(
                "prompt_above_bounds",
                "prompt exceeds the resolved prompt-byte bound",
            ));
        }
        Ok(resolved)
    }

    fn ensure_scope_binding(&self, session_id: &str, workspace: &str) -> Result<(), ErrorEnvelope> {
        if session_id != self.bound_session_id || workspace != self.bound_workspace {
            return Err(ErrorEnvelope {
                code: ErrorCode::ForbiddenScope,
                message: "operation is not bound to this session and workspace".into(),
                request_id: None,
                reason_code: Some("scope_mismatch".into()),
                event_range: None,
            });
        }
        Ok(())
    }

    fn ensure_capability(&self, operation: HeadlessOperation) -> Result<(), ErrorEnvelope> {
        let id = operation.capability_id();
        let Some(descriptor) = self.host.capabilities.get(id) else {
            return Err(forbidden("capability_not_advertised"));
        };
        match descriptor.availability {
            CapabilityAvailability::Available => {}
            // A gated capability needs a separate human grant that this port
            // deliberately cannot issue.
            CapabilityAvailability::Gated => return Err(forbidden("capability_gated")),
            CapabilityAvailability::Unavailable => return Err(forbidden("capability_unavailable")),
        }
        if operation.is_mutating() && !descriptor.mutating {
            return Err(forbidden("capability_not_mutating"));
        }
        Ok(())
    }
}

fn invalid(reason_code: &str, message: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: ErrorCode::InvalidRequest,
        message: message.to_owned(),
        request_id: None,
        reason_code: Some(reason_code.to_owned()),
        event_range: None,
    }
}

fn forbidden(reason_code: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: ErrorCode::ForbiddenScope,
        message: "capability is not available to this consumer".into(),
        request_id: None,
        reason_code: Some(reason_code.to_owned()),
        event_range: None,
    }
}

fn unsupported(operation: HeadlessOperation) -> ErrorEnvelope {
    ErrorEnvelope {
        code: ErrorCode::AuthorityUnavailable,
        message: "operation is not implemented by this headless authority".into(),
        request_id: None,
        reason_code: Some(format!("unimplemented_{}", operation.capability_id())),
        event_range: None,
    }
}
