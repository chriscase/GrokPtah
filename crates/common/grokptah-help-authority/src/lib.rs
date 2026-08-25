//! Action-time authority for GrokPtah Help.
//!
//! Help is a *capability*, not a chat surface. Every retrieval, source read,
//! and bounded answer is authorized here, at the moment of the action, against
//! the principal, tenant, project, and capability supplied with that action —
//! never against a cached session decision.
//!
//! Three properties this crate exists to hold:
//!
//! 1. **Default deny.** A source that is not public is denied unless an
//!    explicit grant names the same tenant and the same scope. A malformed
//!    scope denies; it does not fall back to public.
//! 2. **Closed contracts.** Requests are `deny_unknown_fields`, so a field
//!    this version does not understand is a rejection rather than a silently
//!    dropped restriction.
//! 3. **Non-leaking receipts.** A receipt names ids and digests only. It never
//!    carries a path, a heading, source content, or the query text, so an
//!    audit log cannot become the leak the authority prevented.
//!
//! The crate has no Tauri, filesystem, network, or provider dependency. The
//! desktop command layer and the browser broker both call the same
//! [`authorize`] function, which is what makes their decisions identical by
//! construction rather than by convention.

pub mod contract;
pub mod grant;
pub mod schema;

use sha2::{Digest, Sha256};
use thiserror::Error;

pub use grant::{
    AuthenticatedPrincipal, GrantAcceptance, GrantMintingKey, GrantRejection, HELP_GRANT_SCHEMA,
    HelpGrant, ServedManifest, mint_grant, verify_grant,
};

pub use contract::{
    Action, Capability, DecisionReceipt, DecisionRequest, DecisionResponse, DenyReason,
    HELP_DECISION_REQUEST_SCHEMA, HELP_DECISION_RESPONSE_SCHEMA, Principal, SourceDecision,
    SourceDescriptor, Visibility,
};

/// Bounds. A request may not drive unbounded work or unbounded receipts.
pub const MAX_SOURCES_PER_DECISION: usize = 64;
/// Longest accepted identifier, in bytes.
pub const MAX_ID_BYTES: usize = 256;

/// A request that could not even be evaluated.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum AuthorityError {
    #[error("help authority: request could not be parsed: {0}")]
    Malformed(String),
}

/// Domain-separated, length-prefixed digest.
///
/// Length prefixes make the encoding injective, so no two distinct field lists
/// can hash the same. Joining with a separator does not have that property:
/// a separator inside a field makes two different field lists identical.
pub(crate) fn domain_digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in std::iter::once(&domain).chain(fields.iter()) {
        hasher.update(field.len().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

pub(crate) fn id_within_bounds(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ID_BYTES
}

/// Which capability an action requires before any source is even considered.
fn required_capability(action: Action) -> Capability {
    match action {
        Action::Search | Action::ReadSource => Capability::HelpSearch,
        Action::Answer => Capability::HelpAnswer,
    }
}

/// Decide one source.
///
/// Public sources need only the base capability. Everything else must match
/// the principal's tenant *and* prove the narrower scope, with the matching
/// capability held. A missing owner field denies as `MalformedScope` rather
/// than being treated as unscoped.
fn decide_source(principal: &Principal, source: &SourceDescriptor) -> Option<DenyReason> {
    if !id_within_bounds(&source.source_id) || !id_within_bounds(&source.tenant_id) {
        return Some(DenyReason::Bounds);
    }

    match source.visibility {
        Visibility::Public => None,
        Visibility::Project => {
            // Tenant is checked before scope so a cross-tenant probe cannot
            // learn whether a project id exists.
            if source.tenant_id != principal.tenant_id {
                return Some(DenyReason::TenantMismatch);
            }
            if !principal.holds(Capability::HelpSearchProject) {
                return Some(DenyReason::MissingCapability);
            }
            match source.project_id.as_deref() {
                None => Some(DenyReason::MalformedScope),
                Some(project_id) if !id_within_bounds(project_id) => Some(DenyReason::Bounds),
                Some(project_id) if principal.member_of(project_id) => None,
                Some(_) => Some(DenyReason::ScopeMismatch),
            }
        }
        Visibility::Private => {
            if source.tenant_id != principal.tenant_id {
                return Some(DenyReason::TenantMismatch);
            }
            if !principal.holds(Capability::HelpSearchPrivate) {
                return Some(DenyReason::MissingCapability);
            }
            match source.owner_principal_id.as_deref() {
                None => Some(DenyReason::MalformedScope),
                Some(owner) if !id_within_bounds(owner) => Some(DenyReason::Bounds),
                Some(owner) if owner == principal.principal_id => None,
                Some(_) => Some(DenyReason::ScopeMismatch),
            }
        }
    }
}

/// Longest identifier a receipt will echo back.
///
/// A denial previously cloned whatever `source_id` the caller sent. That let a
/// rejected request write an unbounded, unsanitized string into an audit log —
/// the one place a denial is supposed to make things safer.
pub const MAX_RECEIPT_ID_BYTES: usize = 128;

/// Bound and sanitize an identifier before it enters a receipt.
///
/// Control characters are stripped so a receipt cannot carry a terminal escape
/// or a line break into a log, and the result is truncated to a fixed budget
/// so a receipt's size never depends on the caller.
fn receipt_safe_id(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_RECEIPT_ID_BYTES)
        .collect();
    if cleaned.is_empty() {
        "<unnamed>".to_string()
    } else {
        cleaned
    }
}

fn deny(request: &DecisionRequest, reason: DenyReason) -> DecisionResponse {
    // A denied action denies every source with it: no partial surface leaks
    // out of a request that was not permitted in the first place.
    let denied: Vec<SourceDecision> = request
        .sources
        .iter()
        .take(MAX_SOURCES_PER_DECISION)
        .map(|source| SourceDecision {
            source_id: receipt_safe_id(&source.source_id),
            allowed: false,
            denied_because: Some(reason),
        })
        .collect();
    let receipt = build_receipt_with_reason(request, &[], &denied, None, Some(reason));
    DecisionResponse {
        schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
        allowed: false,
        denied_because: Some(reason),
        receipt,
    }
}

fn build_receipt(
    request: &DecisionRequest,
    allowed_source_ids: &[String],
    denied: &[SourceDecision],
    grant: Option<&HelpGrant>,
) -> DecisionReceipt {
    build_receipt_with_reason(request, allowed_source_ids, denied, grant, None)
}

/// Build a receipt whose digest covers the entire decision.
///
/// The earlier digest covered only the action, the principal, the two index
/// digests, and the bare source ids. Two decisions that allowed the same ids
/// for different reasons — a different capability set, a different project
/// membership, a different visibility, allow versus deny — collided. A receipt
/// that cannot distinguish those is not an audit record.
///
/// Every authority input and every decision output is now length-prefixed into
/// one domain-separated digest, so any difference in identity, membership,
/// visibility, outcome, or reason produces a different value.
fn build_receipt_with_reason(
    request: &DecisionRequest,
    allowed_source_ids: &[String],
    denied: &[SourceDecision],
    grant: Option<&HelpGrant>,
    action_reason: Option<DenyReason>,
) -> DecisionReceipt {
    let action = action_label(request.action);
    let mut owned: Vec<String> = vec![
        action.to_string(),
        request.principal.principal_id.clone(),
        request.principal.tenant_id.clone(),
        request.corpus_digest.clone(),
        request.index_digest.clone(),
        // Outcome, so an allow and a deny over the same ids never collide.
        if action_reason.is_some() {
            "denied"
        } else {
            "allowed"
        }
        .to_string(),
        action_reason.map_or_else(|| "none".to_string(), |reason| format!("{reason:?}")),
    ];

    // Authority identity: capabilities and membership, sorted and counted.
    let mut capabilities: Vec<String> = request
        .principal
        .capabilities
        .iter()
        .map(|capability| format!("{capability:?}"))
        .collect();
    capabilities.sort();
    owned.push(capabilities.len().to_string());
    owned.extend(capabilities);

    let mut projects = request.principal.project_ids.clone();
    projects.sort();
    owned.push(projects.len().to_string());
    owned.extend(projects);

    // Grant identity, when the decision came from one.
    match grant {
        Some(grant) => {
            owned.push("grant".to_string());
            owned.push(grant.grant_id.clone());
            owned.push(grant.policy_revision.clone());
            owned.push(grant.grant_revision.to_string());
            owned.push(format!("{:?}", grant.max_visibility));
            owned.push(grant.manifest_digest.clone());
        }
        None => owned.push("no-grant".to_string()),
    }

    // Source identity: each allowed id carries its visibility and its own
    // digest, so substituting a source's bytes changes the receipt.
    let describe = |source_id: &str| -> (String, String) {
        request
            .sources
            .iter()
            .find(|source| source.source_id == source_id)
            .map_or_else(
                || ("unknown".to_string(), "unknown".to_string()),
                |source| (format!("{:?}", source.visibility), source.digest.clone()),
            )
    };
    owned.push(allowed_source_ids.len().to_string());
    for id in allowed_source_ids {
        let (visibility, digest) = describe(id);
        owned.push(id.clone());
        owned.push(visibility);
        owned.push(digest);
    }
    owned.push(denied.len().to_string());
    for decision in denied {
        let (visibility, digest) = describe(&decision.source_id);
        owned.push(decision.source_id.clone());
        owned.push(visibility);
        owned.push(digest);
        owned.push(
            decision
                .denied_because
                .map_or_else(|| "none".to_string(), |reason| format!("{reason:?}")),
        );
    }

    let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    DecisionReceipt {
        schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
        action: request.action,
        principal_id: receipt_safe_id(&request.principal.principal_id),
        tenant_id: receipt_safe_id(&request.principal.tenant_id),
        corpus_digest: request.corpus_digest.clone(),
        index_digest: request.index_digest.clone(),
        allowed_source_ids: allowed_source_ids
            .iter()
            .map(|id| receipt_safe_id(id))
            .collect(),
        denied: denied.to_vec(),
        receipt_digest: domain_digest("grokptah.help.receipt.v2", &refs),
    }
}

fn action_label(action: Action) -> &'static str {
    match action {
        Action::Search => "search",
        Action::Answer => "answer",
        Action::ReadSource => "read_source",
    }
}

/// Authorize one Help action against the corpus and index actually being served.
///
/// `served_corpus_digest` and `served_index_digest` are what this process is
/// really serving; the request carries what the caller *believes*. A mismatch
/// denies as [`DenyReason::StaleIndex`] rather than answering from a different
/// corpus than the caller reasoned about.
#[must_use]
pub fn authorize(
    request: &DecisionRequest,
    served_corpus_digest: &str,
    served_index_digest: &str,
) -> DecisionResponse {
    if request.schema != HELP_DECISION_REQUEST_SCHEMA {
        return deny(request, DenyReason::UnknownSchema);
    }
    if !id_within_bounds(&request.principal.principal_id)
        || !id_within_bounds(&request.principal.tenant_id)
        || request.sources.len() > MAX_SOURCES_PER_DECISION
    {
        return deny(request, DenyReason::Bounds);
    }
    if request.corpus_digest != served_corpus_digest || request.index_digest != served_index_digest
    {
        return deny(request, DenyReason::StaleIndex);
    }
    if !request.principal.holds(required_capability(request.action)) {
        return deny(request, DenyReason::MissingCapability);
    }

    let mut allowed_source_ids = Vec::new();
    let mut denied = Vec::new();
    for source in &request.sources {
        match decide_source(&request.principal, source) {
            None => allowed_source_ids.push(source.source_id.clone()),
            Some(reason) => denied.push(SourceDecision {
                source_id: source.source_id.clone(),
                allowed: false,
                denied_because: Some(reason),
            }),
        }
    }

    // `ReadSource` names exactly one source and is meaningless if it was
    // denied, so the action fails rather than returning an empty allow.
    if request.action == Action::ReadSource && allowed_source_ids.is_empty() {
        let reason = denied
            .first()
            .and_then(|decision| decision.denied_because)
            .unwrap_or(DenyReason::ScopeMismatch);
        let receipt = build_receipt(request, &[], &denied, None);
        return DecisionResponse {
            schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
            allowed: false,
            denied_because: Some(reason),
            receipt,
        };
    }

    let receipt = build_receipt(request, &allowed_source_ids, &denied, None);
    DecisionResponse {
        schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
        allowed: true,
        denied_because: None,
        receipt,
    }
}

/// What a process is actually serving.
///
/// Held separately from the request, which carries what the caller *believes*.
/// Comparing the two is what makes a stale index detectable instead of
/// silently answering from a different corpus.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ServedIndex {
    pub corpus_digest: String,
    pub index_digest: String,
}

/// Authorize against a served index.
///
/// This is the whole body of the desktop Tauri command and of the browser
/// broker's server side. Keeping it here rather than in either adapter is what
/// makes the two transports identical by construction: an adapter that added
/// logic of its own could diverge even though both authority implementations
/// agree on the shared fixtures.
#[must_use]
pub fn authorize_for_served(request: &DecisionRequest, served: &ServedIndex) -> DecisionResponse {
    authorize(request, &served.corpus_digest, &served.index_digest)
}

/// Authorize an action using a host-minted grant.
///
/// This is the production entry point. Unlike [`authorize`], the caller does
/// not supply a principal, a capability set, a project list, or an index
/// digest — all of those come from the grant the host minted and are verified
/// before anything is evaluated. A renderer therefore cannot authorize itself,
/// and cannot fail open by omitting a field it would rather not mention.
///
/// The sources still come from the caller's *request*, but they are only ever
/// narrowing: each is checked against the grant, and the grant's
/// `max_visibility` caps what any of them can reach.
#[must_use]
pub fn authorize_with_grant(
    key: &GrantMintingKey,
    presented: &HelpGrant,
    acceptance: &GrantAcceptance,
    sources: &[SourceDescriptor],
) -> DecisionResponse {
    // Build the request the grant describes, so a forged or stale grant is
    // rejected before its identity is used for anything.
    let principal = Principal {
        principal_id: presented.principal_id.clone(),
        tenant_id: presented.tenant_id.clone(),
        project_ids: presented.project_ids.clone(),
        capabilities: presented.capabilities.clone(),
    };
    let request = DecisionRequest {
        schema: HELP_DECISION_REQUEST_SCHEMA.to_string(),
        action: acceptance.action,
        principal,
        corpus_digest: acceptance.manifest.corpus_digest.clone(),
        index_digest: acceptance.manifest.index_digest.clone(),
        sources: sources.to_vec(),
    };

    if let Err(rejection) = verify_grant(key, presented, acceptance) {
        let reason = match rejection {
            GrantRejection::UnknownSchema => DenyReason::UnknownSchema,
            GrantRejection::Forged => DenyReason::ForgedGrant,
            GrantRejection::ActionMismatch => DenyReason::ForgedGrant,
            GrantRejection::StaleRevision | GrantRejection::IndexMismatch => DenyReason::StaleIndex,
            GrantRejection::Expired => DenyReason::ExpiredGrant,
            GrantRejection::Bounds => DenyReason::Bounds,
        };
        return deny(&request, reason);
    }

    let mut response = authorize(
        &request,
        &acceptance.manifest.corpus_digest,
        &acceptance.manifest.index_digest,
    );

    // The grant caps visibility regardless of what capabilities it carries:
    // a grant minted for public reach cannot be widened by a request that
    // happens to name a project source.
    if response.allowed && presented.max_visibility != Visibility::Private {
        let capped: Vec<String> = response
            .receipt
            .allowed_source_ids
            .iter()
            .filter(|id| {
                sources
                    .iter()
                    .find(|source| &&source.source_id == id)
                    .is_some_and(|source| {
                        visibility_within(source.visibility, presented.max_visibility)
                    })
            })
            .cloned()
            .collect();
        if capped.len() != response.receipt.allowed_source_ids.len() {
            let withheld: Vec<SourceDecision> = response
                .receipt
                .allowed_source_ids
                .iter()
                .filter(|id| !capped.contains(id))
                .map(|id| SourceDecision {
                    source_id: id.clone(),
                    allowed: false,
                    denied_because: Some(DenyReason::VisibilityCapped),
                })
                .collect();
            let mut denied = response.receipt.denied.clone();
            denied.extend(withheld);
            let receipt = build_receipt(&request, &capped, &denied, Some(presented));
            response = DecisionResponse {
                schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
                allowed: true,
                denied_because: None,
                receipt,
            };
        }
    }
    response
}

/// True when `actual` is no wider than `cap`.
fn visibility_within(actual: Visibility, cap: Visibility) -> bool {
    let rank = |visibility: Visibility| match visibility {
        Visibility::Public => 0,
        Visibility::Project => 1,
        Visibility::Private => 2,
    };
    rank(actual) <= rank(cap)
}

/// Authorize from a JSON payload.
///
/// Parsing is strict: an unknown field is a rejection, not a warning. This is
/// the entry point both the Tauri command layer and the browser broker use,
/// so an unparseable request fails identically on both.
pub fn authorize_json(
    payload: &str,
    served_corpus_digest: &str,
    served_index_digest: &str,
) -> Result<DecisionResponse, AuthorityError> {
    let request: DecisionRequest = serde_json::from_str(payload)
        .map_err(|error| AuthorityError::Malformed(error.to_string()))?;
    Ok(authorize(
        &request,
        served_corpus_digest,
        served_index_digest,
    ))
}

#[cfg(test)]
mod grant_tests;
#[cfg(test)]
mod tests;
