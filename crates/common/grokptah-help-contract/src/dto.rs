//! Every Semantic Help authority, request, result, and receipt type.
//!
//! # The renderer seam
//!
//! The seam between the host and the renderer is enforced by which types can
//! be *deserialized*, not by convention. A Tauri command can only accept what
//! implements [`serde::Deserialize`], so the inbound vocabulary below —
//! [`HelpAsk`], [`HelpFollow`], [`HelpCancelRequest`] — is deliberately the
//! complete set of types the renderer can send. Everything else in this module
//! is `Serialize`-only and therefore un-sendable *by construction*: a renderer
//! cannot mint a [`Grant`], an [`Admission`], a [`Principal`], a capability
//! set, a route, or a transport, because there is no code path that would
//! accept one. `dto_tests::renderer_cannot_mint_authority` pins that set so a
//! later `#[derive(Deserialize)]` on an authority type fails the build.
//!
//! The renderer's whole vocabulary is a question, a locale, and opaque handles
//! the host issued. It never names a chunk, a source, a model, or an endpoint.
//!
//! # Route freedom
//!
//! [`HelpRequest`] carries no route, model, endpoint, or provider field. A
//! request that names its own route is a request that chose its own authority;
//! the host resolves the route from the grant at send time and the choice never
//! appears in a document any other party can influence.

use serde::{Deserialize, Serialize};

use crate::corpus::Visibility;
use crate::digest::{domain, domain_digest};

// ---------------------------------------------------------------------------
// Inbound: the complete set of types a renderer may send.
// ---------------------------------------------------------------------------

/// Ask Help a question. The only content the renderer supplies.
///
/// `session` is an opaque handle the host issued; the renderer cannot forge a
/// principal, tenant, or capability by editing it, because the host resolves
/// it against its own session table and ignores anything it does not know.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpAsk {
    pub session: String,
    pub question: String,
    #[serde(default)]
    pub locale: Option<String>,
}

/// Poll or resume an in-flight ask by its opaque handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpFollow {
    pub session: String,
    pub handle: String,
}

/// Cancel an in-flight ask by its opaque handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelpCancelRequest {
    pub session: String,
    pub handle: String,
}

// ---------------------------------------------------------------------------
// Identity. Host-derived; never inbound.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Anonymous,
    Member,
    Operator,
}

/// Who is asking, as the host determined — not as anyone claimed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Principal {
    pub principal_id: String,
    pub tenant_id: String,
    pub session_id: String,
    pub kind: PrincipalKind,
    /// Capability ids the host resolved for this principal.
    pub capabilities: Vec<String>,
    /// The most permissive source visibility this principal may ever see.
    pub visibility_ceiling: Visibility,
}

// ---------------------------------------------------------------------------
// Manifest. Host-owned and host-filtered.
// ---------------------------------------------------------------------------

/// One article as the host is willing to serve it to a given principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManifestEntry {
    pub article_id: String,
    pub article_digest: String,
    pub chunk_ids: Vec<String>,
    pub source_ids: Vec<String>,
    pub visibility: Visibility,
}

/// The set of content a principal is entitled to, computed by the host.
///
/// The host never receives a manifest. It derives one from the corpus and the
/// principal, so "what may I see" is answered by the party that enforces the
/// answer rather than by the party it constrains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// Monotonic; bumped whenever the corpus or the permission inputs change.
    pub revision: u64,
    pub corpus_digest: String,
    pub source_digest: String,
    pub entries: Vec<ManifestEntry>,
    pub digest: String,
}

impl Manifest {
    /// Recompute this manifest's digest over its exact contents.
    #[must_use]
    pub fn compute_digest(
        revision: u64,
        corpus_digest: &str,
        source_digest: &str,
        entries: &[ManifestEntry],
    ) -> String {
        let revision_text = revision.to_string();
        let mut fields: Vec<&str> = vec![&revision_text, corpus_digest, source_digest];
        let flattened: Vec<String> = entries
            .iter()
            .map(|entry| {
                format!(
                    "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
                    entry.article_id,
                    entry.article_digest,
                    entry.chunk_ids.join(","),
                    entry.source_ids.join(","),
                    entry.visibility.as_str()
                )
            })
            .collect();
        fields.extend(flattened.iter().map(String::as_str));
        domain_digest(domain::MANIFEST, &fields)
    }

    /// Whether this manifest admits `chunk_id`.
    #[must_use]
    pub fn allows_chunk(&self, chunk_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.chunk_ids.iter().any(|id| id == chunk_id))
    }

    /// Whether this manifest admits `source_id`.
    #[must_use]
    pub fn allows_source(&self, source_id: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| entry.source_ids.iter().any(|id| id == source_id))
    }
}

// ---------------------------------------------------------------------------
// Grant. Minted host-side.
// ---------------------------------------------------------------------------

/// Permission to ask one question, bound to everything that made it valid.
///
/// A grant names the exact corpus and manifest it was issued against. If either
/// changes underneath it, the grant no longer matches and the next
/// reauthorization denies rather than serving content the principal was never
/// cleared for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Grant {
    pub grant_id: String,
    pub principal_id: String,
    pub tenant_id: String,
    pub session_id: String,
    pub corpus_digest: String,
    pub manifest_digest: String,
    pub manifest_revision: u64,
    pub visibility_ceiling: Visibility,
    pub capabilities: Vec<String>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub digest: String,
}

impl Grant {
    /// Digest binding a grant to its principal, tenant, corpus and manifest.
    #[must_use]
    pub fn compute_digest(
        grant_id: &str,
        principal_id: &str,
        tenant_id: &str,
        session_id: &str,
        corpus_digest: &str,
        manifest_digest: &str,
        manifest_revision: u64,
        visibility_ceiling: Visibility,
        capabilities: &[String],
        issued_at_ms: u64,
        expires_at_ms: u64,
    ) -> String {
        let revision = manifest_revision.to_string();
        let issued = issued_at_ms.to_string();
        let expires = expires_at_ms.to_string();
        let mut fields: Vec<&str> = vec![
            grant_id,
            principal_id,
            tenant_id,
            session_id,
            corpus_digest,
            manifest_digest,
            &revision,
            visibility_ceiling.as_str(),
            &issued,
            &expires,
        ];
        fields.extend(capabilities.iter().map(String::as_str));
        domain_digest(domain::GRANT, &fields)
    }
}

// ---------------------------------------------------------------------------
// Admission. Always connected to the grant and the exact request.
// ---------------------------------------------------------------------------

/// A grant, a deadline, and the digest of the one request it admits.
///
/// Binding the request digest is what stops a substituted request: an
/// admission issued for question A cannot carry question B to the provider,
/// because the executor recomputes the digest and compares before sending.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Admission {
    pub admission_id: String,
    pub grant_id: String,
    pub grant_digest: String,
    pub request_digest: String,
    pub admitted_at_ms: u64,
    pub deadline_ms: u64,
    pub digest: String,
}

impl Admission {
    #[must_use]
    pub fn compute_digest(
        admission_id: &str,
        grant_id: &str,
        grant_digest: &str,
        request_digest: &str,
        admitted_at_ms: u64,
        deadline_ms: u64,
    ) -> String {
        let admitted = admitted_at_ms.to_string();
        let deadline = deadline_ms.to_string();
        domain_digest(
            domain::ADMISSION,
            &[
                admission_id,
                grant_id,
                grant_digest,
                request_digest,
                &admitted,
                &deadline,
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// Request. Route-free.
// ---------------------------------------------------------------------------

/// One chunk of corpus context, carried with the digest of the bytes it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContextChunk {
    pub chunk_id: String,
    pub chunk_digest: String,
    pub source_ids: Vec<String>,
    pub text: String,
}

/// Exactly what the host will send a provider, and nothing about how.
///
/// There is no route, model, endpoint, tool, history, or workspace field here,
/// and no place to add one without changing this type: the request is a
/// question over named corpus bytes and nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelpRequest {
    pub request_id: String,
    pub corpus_digest: String,
    pub manifest_revision: u64,
    pub question: String,
    pub locale: String,
    pub context: Vec<ContextChunk>,
    pub instruction: String,
    pub digest: String,
}

impl HelpRequest {
    /// Digest over the exact question and the exact context bytes.
    #[must_use]
    pub fn compute_digest(
        request_id: &str,
        corpus_digest: &str,
        manifest_revision: u64,
        question: &str,
        locale: &str,
        context: &[ContextChunk],
        instruction: &str,
    ) -> String {
        let revision = manifest_revision.to_string();
        let mut fields: Vec<&str> = vec![
            request_id,
            corpus_digest,
            &revision,
            question,
            locale,
            instruction,
        ];
        for chunk in context {
            fields.push(&chunk.chunk_id);
            fields.push(&chunk.chunk_digest);
            fields.push(&chunk.text);
        }
        domain_digest(domain::REQUEST, &fields)
    }
}

// ---------------------------------------------------------------------------
// Validated result. Claims and spans are the validator's, never the provider's.
// ---------------------------------------------------------------------------

/// A half-open byte range into one chunk's exact text.
///
/// `start` and `end` are byte offsets that must land on UTF-8 character
/// boundaries of the chunk named by `chunk_id`, whose bytes are pinned by
/// `chunk_digest`. A span is meaningless without that digest: without it the
/// range would follow the name to whatever text answers to it next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CitationSpan {
    pub chunk_id: String,
    pub chunk_digest: String,
    pub source_id: String,
    pub source_digest: String,
    pub start: usize,
    pub end: usize,
}

/// One assertion the validator was able to tie to corpus bytes.
///
/// The provider does not get to declare its own claims: it returns prose, and
/// the validator decides what counts as a claim and which bytes support it.
/// A model that labelled its own output would be grading its own work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claim {
    pub ordinal: usize,
    pub text: String,
    pub spans: Vec<CitationSpan>,
}

/// What the validator removed before anything reached a renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RedactionKind {
    /// Something shaped like a credential.
    Secret,
    /// An absolute or user-home filesystem path.
    Path,
    /// A C0/C1 control character.
    Control,
    /// A bidirectional override that can reorder rendered text.
    Bidi,
    /// Markup or a link target in what must be plain text.
    Markup,
}

impl RedactionKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Path => "path",
            Self::Control => "control",
            Self::Bidi => "bidi",
            Self::Markup => "markup",
        }
    }
}

/// The validated answer: plain text, cited, and stripped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidatedAnswer {
    pub request_id: String,
    pub corpus_digest: String,
    pub claims: Vec<Claim>,
    pub redactions: Vec<RedactionKind>,
    /// Present when the validator could not support the answer at all.
    pub abstained: bool,
}

// ---------------------------------------------------------------------------
// Denial. Precise internally, coarse in public.
// ---------------------------------------------------------------------------

/// Why the host refused. Recorded in the receipt, never sent to a renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The manifest moved on from the revision the grant was issued against.
    StaleRevision,
    /// The grant's expiry has passed.
    Expired,
    /// The grant was explicitly revoked.
    Revoked,
    /// The corpus bytes changed under a grant issued against the old ones.
    SourceDrift,
    /// The grant belongs to a different tenant than the one presenting it.
    CrossTenantReplay,
    /// The request digest does not match the one the admission bound.
    SubstitutedRequest,
    /// No such session, or the session is not the grant's.
    UnknownSession,
    /// The principal lacks a capability the content requires.
    MissingCapability,
    /// The content is more restricted than the principal's ceiling.
    VisibilityCeiling,
    /// The executor is saturated and the queue is full.
    Saturated,
    /// The deadline passed before the work could start.
    DeadlineExceeded,
    /// The renderer asked about a handle that is not its own.
    UnknownHandle,
}

impl DenyReason {
    #[must_use]
    pub const fn as_str(self_: &Self) -> &'static str {
        match self_ {
            Self::StaleRevision => "stale_revision",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::SourceDrift => "source_drift",
            Self::CrossTenantReplay => "cross_tenant_replay",
            Self::SubstitutedRequest => "substituted_request",
            Self::UnknownSession => "unknown_session",
            Self::MissingCapability => "missing_capability",
            Self::VisibilityCeiling => "visibility_ceiling",
            Self::Saturated => "saturated",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::UnknownHandle => "unknown_handle",
        }
    }

    /// The coarse code a renderer is allowed to see.
    ///
    /// Every authorization failure collapses to `not_available`. Telling a
    /// caller *why* it was refused tells it what exists: "revoked" and
    /// "missing capability" distinguish a source that is there from one that
    /// is not, which is exactly the probe a manifest is meant to prevent.
    #[must_use]
    pub const fn public_code(self_: &Self) -> PublicErrorCode {
        match self_ {
            Self::StaleRevision
            | Self::Expired
            | Self::Revoked
            | Self::SourceDrift
            | Self::CrossTenantReplay
            | Self::SubstitutedRequest
            | Self::UnknownSession
            | Self::MissingCapability
            | Self::VisibilityCeiling
            | Self::UnknownHandle => PublicErrorCode::NotAvailable,
            Self::Saturated => PublicErrorCode::Busy,
            Self::DeadlineExceeded => PublicErrorCode::Timeout,
        }
    }
}

/// The complete public error vocabulary. Three codes, no detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicErrorCode {
    /// Help cannot answer this. Covers every authorization outcome.
    NotAvailable,
    /// Try again shortly.
    Busy,
    /// The attempt ran out of time.
    Timeout,
}

impl PublicErrorCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotAvailable => "not_available",
            Self::Busy => "busy",
            Self::Timeout => "timeout",
        }
    }

    /// The fixed, non-identifying message shown for this code.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::NotAvailable => "Help cannot answer that right now.",
            Self::Busy => "Help is busy. Try again shortly.",
            Self::Timeout => "Help took too long and stopped.",
        }
    }
}

// ---------------------------------------------------------------------------
// Receipt. Zero content.
// ---------------------------------------------------------------------------

/// How an attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A validated answer was served.
    Answered,
    /// The validator could not support any claim; nothing was served.
    Abstained,
    /// Authority refused before any provider call.
    Denied,
    /// The caller cancelled and the host observed the provider stop.
    Cancelled,
    /// The provider never reached quiescence. Not the same as cancelled.
    Abandoned,
    /// The deadline elapsed.
    TimedOut,
}

impl Outcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Abstained => "abstained",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
            Self::Abandoned => "abandoned",
            Self::TimedOut => "timed_out",
        }
    }
}

/// Whether the host actually knows a provider request left the process.
///
/// This is reported as observed, never as assumed. A request that may or may
/// not have been delivered is [`SendCertainty::Unknown`], and a receipt that
/// says `Unknown` is the honest record of a host that cannot prove otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SendCertainty {
    /// No provider request was made. Guaranteed by construction, not by hope.
    NotSent,
    /// The provider accepted the request and the host saw it.
    Sent,
    /// The attempt began and the outcome of delivery is not known.
    Unknown,
}

impl SendCertainty {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotSent => "not_sent",
            Self::Sent => "sent",
            Self::Unknown => "unknown",
        }
    }
}

/// How many of each redaction kind fired, without saying what was redacted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedactionCount {
    pub kind: RedactionKind,
    pub count: usize,
}

/// The durable record of one attempt. Contains no content.
///
/// A receipt carries counts, digests, identities, and timings — never the
/// question, the answer, a chunk, a span's text, or a provider reply. A log
/// that quotes the thing it is auditing becomes a second copy of it, outside
/// every control that governed the first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Receipt {
    pub receipt_id: String,
    /// The durable Run this attempt belongs to.
    pub run_id: String,
    pub request_id: String,
    pub principal_id: String,
    pub tenant_id: String,
    pub session_id: String,
    pub corpus_digest: String,
    pub manifest_revision: u64,
    /// Digest of the request, so a receipt can be matched to a request without
    /// either one carrying the other's content.
    pub request_digest: String,
    pub outcome: Outcome,
    pub send_certainty: SendCertainty,
    /// Internal reason. Present on denial; never mapped to a renderer.
    pub deny_reason: Option<DenyReason>,
    pub public_code: Option<PublicErrorCode>,
    pub claim_count: usize,
    pub span_count: usize,
    pub redactions: Vec<RedactionCount>,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub digest: String,
}

impl Receipt {
    #[must_use]
    pub fn compute_digest(
        receipt_id: &str,
        run_id: &str,
        request_id: &str,
        request_digest: &str,
        outcome: Outcome,
        send_certainty: SendCertainty,
        claim_count: usize,
        span_count: usize,
        finished_at_ms: u64,
    ) -> String {
        let claims = claim_count.to_string();
        let spans = span_count.to_string();
        let finished = finished_at_ms.to_string();
        domain_digest(
            domain::RECEIPT,
            &[
                receipt_id,
                run_id,
                request_id,
                request_digest,
                outcome.as_str(),
                send_certainty.as_str(),
                &claims,
                &spans,
                &finished,
            ],
        )
    }
}

// ---------------------------------------------------------------------------
// Projections. What a renderer is handed.
// ---------------------------------------------------------------------------

/// A citation as a renderer sees it: where to look, never a digest to check
/// against a corpus the renderer does not have in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CitationProjection {
    pub source_id: String,
    pub path: String,
    pub heading: String,
    /// The exact quoted bytes, already redacted.
    pub quote: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimProjection {
    pub ordinal: usize,
    pub text: String,
    pub citations: Vec<CitationProjection>,
}

/// The renderer-facing result. Opaque handle, plain text, no authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HelpProjection {
    /// Opaque; meaningful only to the host that issued it.
    pub handle: String,
    pub status: ProjectionStatus,
    pub claims: Vec<ClaimProjection>,
    /// Coarse code when the attempt did not produce claims.
    pub error: Option<PublicErrorCode>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionStatus {
    Queued,
    Running,
    Answered,
    Abstained,
    Unavailable,
}

impl ProjectionStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Answered => "answered",
            Self::Abstained => "abstained",
            Self::Unavailable => "unavailable",
        }
    }
}

/// The zero-content receipt view a surface may render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReceiptProjection {
    pub receipt_id: String,
    pub run_id: String,
    pub outcome: String,
    pub send_certainty: String,
    pub claim_count: usize,
    pub span_count: usize,
    pub redactions: Vec<RedactionCount>,
    pub corpus_digest: String,
    pub manifest_revision: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    pub digest: String,
}

impl From<&Receipt> for ReceiptProjection {
    fn from(receipt: &Receipt) -> Self {
        Self {
            receipt_id: receipt.receipt_id.clone(),
            run_id: receipt.run_id.clone(),
            outcome: receipt.outcome.as_str().to_string(),
            send_certainty: receipt.send_certainty.as_str().to_string(),
            claim_count: receipt.claim_count,
            span_count: receipt.span_count,
            redactions: receipt.redactions.clone(),
            corpus_digest: receipt.corpus_digest.clone(),
            manifest_revision: receipt.manifest_revision,
            started_at_ms: receipt.started_at_ms,
            finished_at_ms: receipt.finished_at_ms,
            digest: receipt.digest.clone(),
        }
    }
}

/// The executor's fixed bounds, so a surface renders honest limits.
///
/// The four `*_enabled` flags are constants rather than configuration. They
/// exist to be rendered and asserted, not to be turned on: there is no code
/// path in the executor that reads a tool, a history, a workspace, or a
/// fallback route, so a `true` here would be a lie a test would catch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct BoundsProjection {
    pub max_concurrency: usize,
    pub max_queued: usize,
    pub deadline_ms: u64,
    pub single_request: bool,
    pub tools_enabled: bool,
    pub history_enabled: bool,
    pub workspace_enabled: bool,
    pub fallback_enabled: bool,
}
