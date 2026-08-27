//! The host-neutral authority seam.
//!
//! [`HeadlessAuthority`] is everything the port needs from a host. It carries
//! no Tauri, HTTP, MCP, or filesystem concept, so the same port drives an
//! in-process desktop runtime, `grokptah-service`, or a deterministic test
//! double. Implementations are adapters over machinery that already exists —
//! they never contain a second send engine.
//!
//! Two rules shape the trait:
//!
//! 1. **Negotiate, then bind.** A [`PortBinding`] can only be minted from a
//!    [`HostNegotiation`], so an embedder cannot fabricate a host identity or
//!    capability revision. The port renegotiates before every operation and
//!    refuses a binding whose host or revision moved.
//! 2. **Recheck at the effect boundary.** Negotiation is not authorization.
//!    Immediately before a durable effect the port calls
//!    [`HeadlessAuthority::authorize_effect`], and the effect methods require
//!    the [`EffectAuthorization`] that call returned. A host that revoked
//!    authority in between therefore stops the effect by construction, not by
//!    convention.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::types::{
    scope_denied, validate_identifier, HostNegotiation, PortDeliveryEvidence, PortError,
    PortErrorCode, PortExecutionMode, PortLimits, PortOperation, PortPrincipal, PortResult,
    PortReviewFacts, PortRunFacts,
};

/// Exact binding of a principal to one session, workspace, host, and
/// capability revision. Every field participates in every operation: a change
/// in any of them invalidates the binding rather than widening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBinding {
    principal: PortPrincipal,
    session_id: Uuid,
    workspace: String,
    host_id: String,
    capability_revision: u64,
    protocol_version: u32,
}

impl PortBinding {
    /// Mint a binding from a completed negotiation.
    ///
    /// `workspace` is the caller's claimed workspace identity as an exact
    /// string. The port compares it verbatim; it never touches the
    /// filesystem, so a binding cannot be widened by a symlink or a
    /// relative-path trick — the host adapter is what canonicalizes and
    /// allowlists.
    pub fn bind(
        negotiation: &HostNegotiation,
        principal: PortPrincipal,
        session_id: Uuid,
        workspace: impl Into<String>,
    ) -> PortResult<Self> {
        if negotiation.protocol_version != super::types::HEADLESS_PORT_PROTOCOL_VERSION {
            return Err(PortError::new(
                PortErrorCode::StaleBinding,
                "host negotiated a different headless port protocol version",
            ));
        }
        negotiation.limits.validate()?;
        let workspace = workspace.into();
        if workspace.trim().is_empty() {
            return Err(PortError::new(
                PortErrorCode::InvalidRequest,
                "workspace identity must not be empty",
            ));
        }
        if workspace.split(['/', '\\']).any(|segment| segment == "..") {
            return Err(PortError::new(
                PortErrorCode::InvalidRequest,
                "workspace identity must not contain traversal segments",
            ));
        }
        if session_id.is_nil() {
            return Err(PortError::new(
                PortErrorCode::InvalidRequest,
                "session identity must not be nil",
            ));
        }
        Ok(Self {
            principal,
            session_id,
            workspace,
            host_id: validate_identifier(negotiation.host_id.clone())?,
            capability_revision: negotiation.capability_revision,
            protocol_version: negotiation.protocol_version,
        })
    }

    pub fn principal(&self) -> &PortPrincipal {
        &self.principal
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn capability_revision(&self) -> u64 {
        self.capability_revision
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    /// Fail closed when the live host is not the one this binding names.
    pub(crate) fn require_current(&self, negotiation: &HostNegotiation) -> PortResult<()> {
        let stale = PortError::new(
            PortErrorCode::StaleBinding,
            "host identity, protocol version, or capability revision changed; renegotiate and rebind",
        );
        if negotiation.protocol_version != self.protocol_version
            || negotiation.host_id != self.host_id
            || negotiation.capability_revision != self.capability_revision
        {
            return Err(stale);
        }
        Ok(())
    }
}

/// A one-use authorization for exactly one effect on exactly one binding.
///
/// The port cannot construct this type; only a host adapter can, and only
/// from [`HeadlessAuthority::authorize_effect`]. Effect methods take it by
/// value, so a stale authorization cannot be replayed into a second effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectAuthorization {
    operation: PortOperation,
    session_id: Uuid,
    workspace: String,
    capability_revision: u64,
    principal_id: String,
    authorized_at: DateTime<Utc>,
}

impl EffectAuthorization {
    /// Issued by a host adapter after it has rechecked its own authority.
    pub fn issue(
        binding: &PortBinding,
        operation: PortOperation,
        authorized_at: DateTime<Utc>,
    ) -> Self {
        Self {
            operation,
            session_id: binding.session_id(),
            workspace: binding.workspace().to_string(),
            capability_revision: binding.capability_revision(),
            principal_id: binding.principal().principal_id.clone(),
            authorized_at,
        }
    }

    pub fn operation(&self) -> PortOperation {
        self.operation
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    pub fn capability_revision(&self) -> u64 {
        self.capability_revision
    }

    pub fn principal_id(&self) -> &str {
        &self.principal_id
    }

    pub fn authorized_at(&self) -> DateTime<Utc> {
        self.authorized_at
    }

    /// The authorization must describe the same effect on the same binding it
    /// is about to be spent on.
    pub(crate) fn matches(&self, binding: &PortBinding, operation: PortOperation) -> bool {
        self.operation == operation
            && self.session_id == binding.session_id()
            && self.workspace == binding.workspace()
            && self.capability_revision == binding.capability_revision()
            && self.principal_id == binding.principal().principal_id
    }
}

/// One durable page of a run's journal, as supplied by the host.
///
/// `kinds` carries classified event kinds only. The host adapter maps its own
/// event stream into [`super::projection::PortEventKind`], which has no field
/// able to hold text, so no prompt, path, command, tool body, provider
/// payload, or model output can ride a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortEventFacts {
    pub entries: Vec<(u64, super::projection::PortEventKind)>,
    pub next_cursor: Option<u64>,
    pub cursor_expired: bool,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
}

/// Host-neutral effect and read surface backing the headless port.
///
/// Read methods take `&PortBinding` and must apply the host's own scope gate.
/// Effect methods additionally take an [`EffectAuthorization`] minted for that
/// exact operation.
#[async_trait]
pub trait HeadlessAuthority: Send + Sync {
    /// Declared host identity, capability revision, and limits for this
    /// principal, read fresh. The port calls this before every operation.
    async fn negotiate(&self, principal: &PortPrincipal) -> PortResult<HostNegotiation>;

    /// Recheck authority immediately before an effect. Returning an error
    /// stops the effect; there is no other path to an [`EffectAuthorization`].
    async fn authorize_effect(
        &self,
        binding: &PortBinding,
        operation: PortOperation,
    ) -> PortResult<EffectAuthorization>;

    /// Durable evidence for one mutation request id. Used to classify
    /// delivery **without** performing or replaying the effect.
    async fn delivery_evidence(
        &self,
        binding: &PortBinding,
        request_id: &str,
    ) -> PortResult<PortDeliveryEvidence>;

    /// Perform the submit through the host's existing run engine.
    ///
    /// The host writes its durable claim ahead of the effect and settles it
    /// afterwards; the port never re-sends on its behalf.
    #[allow(clippy::too_many_arguments)] // Keeps the submit effect's scope explicit at the port boundary.
    async fn perform_submit(
        &self,
        binding: &PortBinding,
        authorization: EffectAuthorization,
        request_id: &str,
        prompt: &str,
        limits: &PortLimits,
        execution_mode: PortExecutionMode,
        allow_queue: bool,
    ) -> PortResult<PortRunFacts>;

    /// Perform the cancel through the host's existing run engine.
    async fn perform_cancel(
        &self,
        binding: &PortBinding,
        authorization: EffectAuthorization,
        request_id: &str,
        run_id: &str,
    ) -> PortResult<PortRunFacts>;

    /// Durable run facts for one owned, bound run. Unknown, cross-session,
    /// cross-workspace, and malformed ids must all return [`scope_denied`].
    async fn run_facts(&self, binding: &PortBinding, run_id: &str) -> PortResult<PortRunFacts>;

    /// One bounded page of the run's durable journal.
    async fn run_events(
        &self,
        binding: &PortBinding,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> PortResult<PortEventFacts>;

    /// Review facts for a reviewable isolated run.
    async fn review_facts(
        &self,
        binding: &PortBinding,
        run_id: &str,
    ) -> PortResult<PortReviewFacts>;
}

/// Shared helper for adapters: the single scope failure every run-dependent
/// read must produce.
pub fn denied_run_scope() -> PortError {
    scope_denied()
}
