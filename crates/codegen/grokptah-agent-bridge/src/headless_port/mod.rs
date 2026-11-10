//! Headless agent port: a host-neutral embedding surface for other products.
//!
//! ContextDesk — or any other embedder — needs to drive an existing GrokPtah
//! agent runtime without becoming a desktop, an MCP client, or a second owner
//! of durable state. This module is that surface. It is **additive**: it adds
//! no execution path, no persistence, and no provider behaviour. Every effect
//! is delegated to the orchestration runtime that already ships, through
//! [`orchestration_authority::OrchestrationAuthority`].
//!
//! # Scope
//!
//! Exactly four operations:
//!
//! | Operation | Kind | Returns |
//! |---|---|---|
//! | [`port::HeadlessAgentPort::submit`] | mutation | delivery receipt + run projection |
//! | [`port::HeadlessAgentPort::events`] | read | bounded classified page + run projection |
//! | [`port::HeadlessAgentPort::review`] | read | promotion state, fingerprints, counts |
//! | [`port::HeadlessAgentPort::cancel`] | mutation | delivery receipt + run projection |
//!
//! # Guarantees
//!
//! * **Exact binding.** A [`authority::PortBinding`] names one principal, one
//!   session, one workspace identity, one host, and one capability revision,
//!   and can only be minted from a completed negotiation.
//! * **Renegotiation before every operation.** Host identity, declared
//!   capabilities, and limits are read fresh on every call; a moved host or
//!   revision fails closed as `stale_binding` instead of running under stale
//!   authority. Limits are applied from the fresh negotiation, never from bind
//!   time.
//! * **Authorization recheck at the effect boundary.** Immediately before a
//!   durable effect the host rechecks its own scope gate and issues a one-use
//!   [`authority::EffectAuthorization`], which the effect call consumes. The
//!   port cannot construct one, so an effect without a live recheck does not
//!   type-check.
//! * **Durable write-ahead / act / log / acknowledge.** `unknown`, `sending`,
//!   and `uncertain` are visible delivery states, not hidden retries. An
//!   unsettled or interrupted claim is reported as-is and is **never**
//!   auto-replayed; `uncertain` requires a new request id.
//! * **Typed evidence for terminal completion.** A completed run is presented
//!   as verified only when durable typed evidence supports it; otherwise the
//!   projection says `completed_unverified` and names the gaps.
//! * **Principal-scoped reads.** Unknown, cross-session, cross-workspace, and
//!   malformed resources produce one identical `forbidden_scope` failure, so a
//!   read is not an existence oracle.
//! * **Monotonic bounded cursors.** Pages are clamped to the negotiated limit
//!   and the absolute ceiling, sequences strictly increase above the requested
//!   cursor, and an expired cursor is reported as an empty gap rather than a
//!   short stream.
//! * **Redaction by construction.** Prompts, model output, filesystem paths,
//!   tool input and output, credentials, and provider payloads have no field
//!   anywhere in [`projection`] or [`types`] that could hold them.
//!
//! # Deliberate non-goals
//!
//! No Computer Use, no desktop UI, no provider wire, no second send engine, no
//! new durable store, and no published crate. Per ADR-002 §7 this stays an
//! internal module until a second named consumer exercises it in running code.
//!
//! See `docs/HEADLESS_AGENT_PORT.md` for the versioned embedding contract.

pub mod authority;
pub mod orchestration_authority;
pub mod port;
pub mod projection;
pub mod types;

#[cfg(test)]
mod fake;
#[cfg(test)]
mod tests;

pub use authority::{
    denied_run_scope, EffectAuthorization, HeadlessAuthority, PortBinding, PortEventFacts,
};
pub use orchestration_authority::{orchestration_port, OrchestrationAuthority};
pub use port::{port_now, HeadlessAgentPort};
pub use projection::{
    classify_update, evidence_gaps, project_review, project_run_at, run_outcome, PortCancelView,
    PortEvent, PortEventKind, PortEventPage, PortEventRange, PortEventsView, PortReviewProjection,
    PortRunOutcome, PortRunProjection, PortSubmitView,
};
pub use types::{
    scope_denied, HostNegotiation, PortCancelReceipt, PortClaimEvidence, PortClaimState,
    PortDelivery, PortDeliveryEvidence, PortError, PortErrorCode, PortEvidenceGap,
    PortEvidenceSummary, PortExecutionMode, PortHostKind, PortLimits, PortOperation, PortPrincipal,
    PortPromotionState, PortResult, PortReviewFacts, PortRunBounds, PortRunFacts, PortRunState,
    PortStopCause, PortSubmitReceipt, PortSubmitRequest, PortTier, PortVerification,
    DEFAULT_PORT_EVENT_PAGE, HEADLESS_PORT_PROTOCOL_VERSION, HEADLESS_PORT_SCHEMA,
    MAX_PORT_EVENT_PAGE, MAX_PORT_ID_BYTES,
};
