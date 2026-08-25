//! Closed wire contracts for Help authority.
//!
//! Every type here is `deny_unknown_fields`. A caller that sends a field this
//! version does not know about is rejected rather than having it dropped: a
//! silently ignored `visibility` or `capability` field is exactly how a
//! default-deny boundary turns into an allow-by-omission.

use serde::{Deserialize, Serialize};

/// Wire schema id for a decision request.
pub const HELP_DECISION_REQUEST_SCHEMA: &str = "grokptah.help-authority-request.v1";
/// Wire schema id for a decision response.
pub const HELP_DECISION_RESPONSE_SCHEMA: &str = "grokptah.help-authority-response.v1";

/// Who a Help source may be surfaced to.
///
/// `Project` and `Private` are default-deny: reaching them requires an
/// explicit action-time grant naming the same tenant, and for `Project` the
/// same project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Public,
    Project,
    Private,
}

impl Visibility {
    /// True when reaching this source needs an explicit grant.
    #[must_use]
    pub fn requires_grant(self) -> bool {
        !matches!(self, Visibility::Public)
    }
}

/// A capability a principal may hold for Help.
///
/// Deliberately narrow. Help never carries run, promotion, or Computer Use
/// authority, so those capabilities have no representation here at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Search the public corpus.
    HelpSearch,
    /// Search sources scoped to a project the principal is a member of.
    HelpSearchProject,
    /// Search sources scoped to the principal alone.
    HelpSearchPrivate,
    /// Ask the bounded, cited Help answer contract.
    HelpAnswer,
}

/// The action being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Search,
    Answer,
    /// Read the full text of one already-retrieved source.
    ReadSource,
}

/// The authenticated caller.
///
/// There is no "anonymous" variant and no default: a principal must always be
/// named. `project_ids` is what the principal is a member of *now*, supplied
/// by the caller at action time rather than cached in a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub principal_id: String,
    pub tenant_id: String,
    #[serde(default)]
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
}

impl Principal {
    #[must_use]
    pub fn holds(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    #[must_use]
    pub fn member_of(&self, project_id: &str) -> bool {
        self.project_ids.iter().any(|id| id == project_id)
    }
}

/// A source the caller wants surfaced, as the index knows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDescriptor {
    pub source_id: String,
    pub visibility: Visibility,
    /// Owning tenant. Must equal the principal's tenant for any non-public source.
    pub tenant_id: String,
    /// Owning project, required when `visibility` is `project`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Owning principal, required when `visibility` is `private`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_principal_id: Option<String>,
    /// Digest of the source record, carried so a receipt can name it without
    /// ever carrying its path or content.
    pub digest: String,
}

/// One authorization request. Evaluated whole; no partial application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    pub schema: String,
    pub action: Action,
    pub principal: Principal,
    /// Corpus digest the caller believes it is querying.
    pub corpus_digest: String,
    /// Index digest the caller believes it is querying.
    pub index_digest: String,
    /// Candidate sources. May be empty for a search over the public corpus.
    #[serde(default)]
    pub sources: Vec<SourceDescriptor>,
}

/// Why a source or an action was denied.
///
/// Reasons are coarse on purpose. A precise reason such as "this project
/// exists but you are not a member" is itself an information leak.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The wire schema was not the expected one.
    UnknownSchema,
    /// The principal lacks the capability this action requires.
    MissingCapability,
    /// The source belongs to a different tenant.
    TenantMismatch,
    /// The source is scoped to a project or principal the caller is not.
    ScopeMismatch,
    /// A `project`/`private` source did not name its owning scope.
    MalformedScope,
    /// The caller's corpus or index digest is not the one being served.
    StaleIndex,
    /// A bound was exceeded (source count, id length).
    Bounds,
    /// The presented grant was not minted by this host, or was edited.
    ForgedGrant,
    /// The presented grant is outside its validity window.
    ExpiredGrant,
    /// Allowed by capability, but wider than the grant's visibility cap.
    VisibilityCapped,
    /// The presented source record is not the one the manifest holds.
    SourceDigestMismatch,
}

/// Per-source outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceDecision {
    pub source_id: String,
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_because: Option<DenyReason>,
}

/// A bounded, non-leaking record of one decision.
///
/// Carries ids and digests only. No path, no heading, no content, no query
/// text, and no principal-supplied strings beyond the ids that were checked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionReceipt {
    pub schema: String,
    pub action: Action,
    pub principal_id: String,
    pub tenant_id: String,
    pub corpus_digest: String,
    pub index_digest: String,
    pub allowed_source_ids: Vec<String>,
    pub denied: Vec<SourceDecision>,
    /// Digest over the decision inputs and outputs, for audit correlation.
    pub receipt_digest: String,
}

/// The whole decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionResponse {
    pub schema: String,
    /// True only when the action itself is permitted. Individual sources may
    /// still be denied; see `receipt.denied`.
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_because: Option<DenyReason>,
    pub receipt: DecisionReceipt,
}
