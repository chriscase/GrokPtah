//! Host admission of one answer route for one exact request.
//!
//! The renderer used to build its own route and hash its own fields:
//!
//! ```text
//! routeDigest = digest({ providerId, tenantId, modelId })
//! ```
//!
//! That digest is self-consistent for *any* values the caller chooses. It
//! proves the fields were not edited after the caller picked them; it says
//! nothing about whether the host would have allowed them. A caller wanting a
//! different provider simply named a different one and hashed that.
//!
//! An admission is minted here instead, under the host's key, and binds:
//!
//! - the route: provider, tenant, project, model;
//! - the authority state it was minted under: grant revision, policy revision;
//! - what is being served: corpus, index, and manifest digests;
//! - **the digest of the exact request body it admits.**
//!
//! The last one is what stops replay. An admission obtained for a harmless
//! question cannot be reattached to a different question, because the request
//! digest it carries would no longer match.
//!
//! After the provider replies, [`bind_outcome`] digests the admission identity
//! together with the accepted answer and its citations. That value is what an
//! audit correlates against, and it cannot be produced without having gone
//! through validation.

use serde::{Deserialize, Serialize};

use crate::grant::{GrantMintingKey, constant_time_eq, hmac_sha256};
use crate::{MAX_ID_BYTES, domain_digest, id_within_bounds};

/// Wire schema id for an admission.
pub const HELP_ADMISSION_SCHEMA: &str = "grokptah.help-answer-admission.v1";

/// Longest admission validity, in milliseconds.
pub const MAX_ADMISSION_LIFETIME_MS: u64 = 120_000;

/// Where an admitted answer may be sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerRoute {
    pub provider_id: String,
    pub tenant_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub model_id: String,
}

/// A host's decision that one request may go to one route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerAdmission {
    pub schema: String,
    pub admission_id: String,
    pub route: AnswerRoute,
    pub grant_revision: u64,
    pub policy_revision: String,
    pub corpus_digest: String,
    pub index_digest: String,
    pub manifest_digest: String,
    /// Digest of the request body this admission is valid for, and no other.
    pub request_digest: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub mac: String,
}

/// What the host is serving and enforcing at the moment of dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionExpectation {
    pub corpus_digest: String,
    pub index_digest: String,
    pub manifest_digest: String,
    pub current_revision: u64,
    pub policy_revision: String,
    /// Digest recomputed from the request body actually being dispatched.
    pub request_digest: String,
    pub now_ms: u64,
}

/// Why an admission was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRejection {
    /// The wire schema was not this one.
    UnknownSchema,
    /// The MAC did not verify: not minted here, or edited after minting.
    Forged,
    /// The admission is for a different request body.
    RequestMismatch,
    /// The grant or policy revision has moved on.
    StaleRevision,
    /// The corpus, index, or manifest being served is not the admitted one.
    IndexMismatch,
    /// Outside the validity window.
    Expired,
    /// An identifier was empty, oversized, or the lifetime was too long.
    Bounds,
}

impl std::fmt::Display for AdmissionRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::UnknownSchema => "unknown admission schema",
            Self::Forged => "admission was not minted by this host",
            Self::RequestMismatch => "admission is for a different request",
            Self::StaleRevision => "admission is for a superseded revision",
            Self::IndexMismatch => "admission does not match what is served",
            Self::Expired => "admission is outside its validity window",
            Self::Bounds => "admission field out of bounds",
        };
        formatter.write_str(text)
    }
}

impl std::error::Error for AdmissionRejection {}

/// Encode an optional field injectively.
///
/// A sentinel string is not enough: `Some("<none>")` and `None` both render as
/// `<none>`, so an admission scoped to a project literally named that would
/// MAC identically to one scoped to no project at all. A separate presence
/// discriminant keeps the two apart, whatever the value happens to spell.
fn optional_fields(value: Option<&str>) -> [String; 2] {
    match value {
        Some(text) => ["present".to_string(), text.to_string()],
        None => ["absent".to_string(), String::new()],
    }
}

/// Fields the MAC covers.
fn mac_fields(admission: &AnswerAdmission) -> Vec<String> {
    let project = optional_fields(admission.route.project_id.as_deref());
    vec![
        admission.schema.clone(),
        admission.admission_id.clone(),
        admission.route.provider_id.clone(),
        admission.route.tenant_id.clone(),
        project[0].clone(),
        project[1].clone(),
        admission.route.model_id.clone(),
        admission.grant_revision.to_string(),
        admission.policy_revision.clone(),
        admission.corpus_digest.clone(),
        admission.index_digest.clone(),
        admission.manifest_digest.clone(),
        admission.request_digest.clone(),
        admission.issued_at_ms.to_string(),
        admission.expires_at_ms.to_string(),
    ]
}

fn admission_mac(key: &GrantMintingKey, admission: &AnswerAdmission) -> String {
    let fields = mac_fields(admission);
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    // A different domain from the grant, so a grant MAC under the same key can
    // never verify here.
    hmac_sha256(key, &domain_digest("grokptah.help.admission.v1", &refs))
}

/// Derive the admission id from what it admits.
///
/// Deterministic rather than random: the same request admitted twice under the
/// same authority state gets the same id, so a duplicate dispatch is visible
/// as a duplicate instead of looking like two independent answers.
fn admission_id(
    route: &AnswerRoute,
    request_digest: &str,
    revision: u64,
    issued_at_ms: u64,
) -> String {
    let project = optional_fields(route.project_id.as_deref());
    domain_digest(
        "grokptah.help.admission-id.v1",
        &[
            &route.provider_id,
            &route.tenant_id,
            &project[0],
            &project[1],
            &route.model_id,
            request_digest,
            &revision.to_string(),
            &issued_at_ms.to_string(),
        ],
    )
}

/// Mint an admission. Host-side only; the renderer never reaches this.
///
/// # Errors
/// Returns [`AdmissionRejection::Bounds`] for an empty or oversized identifier,
/// or a lifetime beyond [`MAX_ADMISSION_LIFETIME_MS`].
#[allow(clippy::too_many_arguments)]
pub fn mint_admission(
    key: &GrantMintingKey,
    route: &AnswerRoute,
    request_digest: &str,
    corpus_digest: &str,
    index_digest: &str,
    manifest_digest: &str,
    grant_revision: u64,
    policy_revision: &str,
    issued_at_ms: u64,
    lifetime_ms: u64,
) -> Result<AnswerAdmission, AdmissionRejection> {
    if !id_within_bounds(&route.provider_id)
        || !id_within_bounds(&route.tenant_id)
        || !id_within_bounds(&route.model_id)
        || !id_within_bounds(policy_revision)
        || request_digest.is_empty()
        || request_digest.len() > MAX_ID_BYTES
        || route
            .project_id
            .as_deref()
            .is_some_and(|id| !id_within_bounds(id))
    {
        return Err(AdmissionRejection::Bounds);
    }
    if lifetime_ms == 0 || lifetime_ms > MAX_ADMISSION_LIFETIME_MS {
        return Err(AdmissionRejection::Bounds);
    }

    let mut admission = AnswerAdmission {
        schema: HELP_ADMISSION_SCHEMA.to_string(),
        admission_id: admission_id(route, request_digest, grant_revision, issued_at_ms),
        route: route.clone(),
        grant_revision,
        policy_revision: policy_revision.to_string(),
        corpus_digest: corpus_digest.to_string(),
        index_digest: index_digest.to_string(),
        manifest_digest: manifest_digest.to_string(),
        request_digest: request_digest.to_string(),
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(lifetime_ms),
        mac: String::new(),
    };
    admission.mac = admission_mac(key, &admission);
    Ok(admission)
}

/// Verify an admission against what is being served right now.
///
/// The MAC is checked first. Everything after it is a comparison between two
/// values the host itself produced, which is only meaningful once the
/// admission is known to be the host's.
///
/// # Errors
/// Returns the first [`AdmissionRejection`] that applies.
pub fn verify_admission(
    key: &GrantMintingKey,
    admission: &AnswerAdmission,
    expectation: &AdmissionExpectation,
) -> Result<(), AdmissionRejection> {
    if admission.schema != HELP_ADMISSION_SCHEMA {
        return Err(AdmissionRejection::UnknownSchema);
    }
    if !constant_time_eq(&admission.mac, &admission_mac(key, admission)) {
        return Err(AdmissionRejection::Forged);
    }
    if !constant_time_eq(&admission.request_digest, &expectation.request_digest) {
        return Err(AdmissionRejection::RequestMismatch);
    }
    if admission.grant_revision != expectation.current_revision
        || admission.policy_revision != expectation.policy_revision
    {
        return Err(AdmissionRejection::StaleRevision);
    }
    if admission.corpus_digest != expectation.corpus_digest
        || admission.index_digest != expectation.index_digest
        || admission.manifest_digest != expectation.manifest_digest
    {
        return Err(AdmissionRejection::IndexMismatch);
    }
    if expectation.now_ms < admission.issued_at_ms || expectation.now_ms >= admission.expires_at_ms
    {
        return Err(AdmissionRejection::Expired);
    }
    Ok(())
}

/// One accepted citation, reduced to what the binding covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BoundCitation {
    /// Index of the answer claim this citation is evidence for.
    pub claim_index: u32,
    pub chunk_id: String,
    pub chunk_digest: String,
    pub source_id: String,
    /// UTF-8 byte offsets into the chunk.
    pub start_utf8: u32,
    pub end_utf8: u32,
}

/// Digest the accepted outcome against the admission that produced it.
///
/// Computed only after validation, so its existence is evidence validation
/// ran. Citations are digested in the order they were accepted, each with its
/// claim index and byte range, so a reordering or a re-pointed span produces a
/// different value.
#[must_use]
pub fn bind_outcome(
    admission: &AnswerAdmission,
    answer: &str,
    uncertainty: &str,
    citations: &[BoundCitation],
) -> String {
    let project = optional_fields(admission.route.project_id.as_deref());
    let mut fields: Vec<String> = vec![
        admission.admission_id.clone(),
        admission.request_digest.clone(),
        admission.route.provider_id.clone(),
        admission.route.tenant_id.clone(),
        project[0].clone(),
        project[1].clone(),
        admission.route.model_id.clone(),
        admission.grant_revision.to_string(),
        admission.policy_revision.clone(),
        admission.corpus_digest.clone(),
        admission.index_digest.clone(),
        admission.manifest_digest.clone(),
        answer.to_string(),
        uncertainty.to_string(),
        citations.len().to_string(),
    ];
    for citation in citations {
        fields.push(citation.claim_index.to_string());
        fields.push(citation.chunk_id.clone());
        fields.push(citation.chunk_digest.clone());
        fields.push(citation.source_id.clone());
        fields.push(citation.start_utf8.to_string());
        fields.push(citation.end_utf8.to_string());
    }
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    domain_digest("grokptah.help.answer-outcome.v1", &refs)
}

/// True when any two citations claim overlapping bytes of the same chunk.
///
/// Coverage is over distinct source bytes. Two citations quoting the same
/// passage are one piece of evidence presented twice, and counting them twice
/// is how a support requirement gets satisfied by repetition.
#[must_use]
pub fn citations_overlap(citations: &[BoundCitation]) -> bool {
    for (position, left) in citations.iter().enumerate() {
        for right in &citations[position + 1..] {
            if left.chunk_id == right.chunk_id
                && left.start_utf8 < right.end_utf8
                && right.start_utf8 < left.end_utf8
            {
                return true;
            }
        }
    }
    false
}
