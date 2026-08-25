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
pub mod schema;

use sha2::{Digest, Sha256};
use thiserror::Error;

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
fn domain_digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in std::iter::once(&domain).chain(fields.iter()) {
        hasher.update(field.len().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn id_within_bounds(value: &str) -> bool {
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

fn deny(request: &DecisionRequest, reason: DenyReason) -> DecisionResponse {
    // A denied action denies every source with it: no partial surface leaks
    // out of a request that was not permitted in the first place.
    let denied: Vec<SourceDecision> = request
        .sources
        .iter()
        .take(MAX_SOURCES_PER_DECISION)
        .map(|source| SourceDecision {
            source_id: source.source_id.clone(),
            allowed: false,
            denied_because: Some(reason),
        })
        .collect();
    let receipt = build_receipt(request, &[], &denied);
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
) -> DecisionReceipt {
    let action = match request.action {
        Action::Search => "search",
        Action::Answer => "answer",
        Action::ReadSource => "read_source",
    };
    let mut fields: Vec<&str> = vec![
        action,
        &request.principal.principal_id,
        &request.principal.tenant_id,
        &request.corpus_digest,
        &request.index_digest,
    ];
    for id in allowed_source_ids {
        fields.push(id);
    }
    for decision in denied {
        fields.push(&decision.source_id);
    }
    DecisionReceipt {
        schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
        action: request.action,
        principal_id: request.principal.principal_id.clone(),
        tenant_id: request.principal.tenant_id.clone(),
        corpus_digest: request.corpus_digest.clone(),
        index_digest: request.index_digest.clone(),
        allowed_source_ids: allowed_source_ids.to_vec(),
        denied: denied.to_vec(),
        receipt_digest: domain_digest("grokptah.help.receipt.v1", &fields),
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
        let receipt = build_receipt(request, &[], &denied);
        return DecisionResponse {
            schema: HELP_DECISION_RESPONSE_SCHEMA.to_string(),
            allowed: false,
            denied_because: Some(reason),
            receipt,
        };
    }

    let receipt = build_receipt(request, &allowed_source_ids, &denied);
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
mod tests;
