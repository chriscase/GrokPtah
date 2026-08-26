//! Strict, source-bound, one-shot Help authority DTOs.
//!
//! The SDK owns only wire shape and validation. It never owns provider
//! credentials, sessions, workspaces, tools, persistence, or execution.

#![allow(missing_docs)]

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest as ShaDigest, Sha256};

pub const HELP_AUTHORITY_SCHEMA: &str = "grokptah.help-authority.v1";
pub const HELP_AUTHORITY_MAX_REQUEST_BYTES: usize = 32_768;
pub const HELP_AUTHORITY_MAX_RESPONSE_BYTES: usize = 32_768;
pub const HELP_AUTHORITY_MAX_CLEANUP_BYTES: usize = 8_192;
pub const HELP_AUTHORITY_MAX_CONTEXT_CHUNKS: usize = 8;
pub const HELP_AUTHORITY_MAX_CITATIONS: usize = 16;
pub const HELP_AUTHORITY_MAX_CLAIMS: usize = 32;
pub const HELP_AUTHORITY_MAX_DURATION_MS: u64 = 20_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpAccessMode {
    Public,
    Authorized,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpAccess {
    Public,
    Gated,
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpDialect {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
    BrokerNative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpMessageKind {
    Request,
    Response,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpAuthorization {
    pub mode: HelpAccessMode,
    pub authorized_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpIdentity {
    pub corpus_digest: String,
    pub source_digest: String,
    pub model_digest: String,
    pub model_id: String,
    pub model_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpProvider {
    pub profile: String,
    pub tenant: String,
    pub model: String,
    pub route_revision: String,
    pub dialect: HelpDialect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpDeadline {
    pub deadline_at: String,
    pub max_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpSourceBinding {
    pub source_id: String,
    pub source_section_digest: String,
    pub source_byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpContextChunk {
    pub chunk_id: String,
    pub article_id: String,
    pub access: HelpAccess,
    pub required_capabilities: Vec<String>,
    pub text: String,
    pub text_digest: String,
    pub span_start: usize,
    pub span_end: usize,
    pub source_bindings: Vec<HelpSourceBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpAuthorityRequest {
    pub schema: String,
    pub kind: HelpMessageKind,
    pub request_id: String,
    pub authorization: HelpAuthorization,
    pub identity: HelpIdentity,
    pub provider: HelpProvider,
    pub deadline: HelpDeadline,
    pub query: String,
    pub context: Vec<HelpContextChunk>,
    pub tools_disabled: bool,
    pub conversation_disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpClaim {
    pub claim_id: String,
    pub text: String,
    pub span_start: usize,
    pub span_end: usize,
    pub citation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpCitation {
    pub citation_id: String,
    pub chunk_id: String,
    pub article_id: String,
    pub span_start: usize,
    pub span_end: usize,
    pub quoted_text: String,
    pub quoted_text_hash: String,
    pub source_id: String,
    pub source_section_digest: String,
    pub claim_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpArtifactCounts {
    pub chat: u8,
    pub session: u8,
    pub transcript: u8,
    pub tool: u8,
    pub workspace: u8,
}

impl Default for HelpArtifactCounts {
    fn default() -> Self {
        Self {
            chat: 0,
            session: 0,
            transcript: 0,
            tool: 0,
            workspace: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpCleanupStatus {
    Finalized,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpProviderTask {
    Joined,
    NotJoined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelpQueueSlot {
    Released,
    NotReleased,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpCleanupReceipt {
    pub schema: String,
    pub kind: HelpMessageKind,
    pub request_id: String,
    pub status: HelpCleanupStatus,
    pub provider_task: HelpProviderTask,
    pub abort_requested: bool,
    pub queue_slot: HelpQueueSlot,
    pub artifact_counts: HelpArtifactCounts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HelpAuthorityResponse {
    pub schema: String,
    pub kind: HelpMessageKind,
    pub request_id: String,
    pub identity: HelpIdentity,
    pub provider: HelpProvider,
    pub deadline: HelpDeadline,
    pub answer: String,
    pub claims: Vec<HelpClaim>,
    pub citations: Vec<HelpCitation>,
    pub uncertainty: String,
    pub cleanup: HelpCleanupReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpParseError {
    pub reason: &'static str,
}

impl fmt::Display for HelpParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for HelpParseError {}

fn error(reason: &'static str) -> HelpParseError {
    HelpParseError { reason }
}

fn safe_text(value: &str, max_bytes: usize) -> bool {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    [
        "authorization:",
        "bearer ",
        "xai-",
        "sk-",
        "api_key",
        "api-key",
        "private key",
        "grokptah_home",
        "clipboard",
        "/users/",
        "/private/",
        "/home/",
    ]
    .iter()
    .all(|needle| !lower.contains(needle))
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("sha256:")
        && value[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sha256_digest(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn valid_id(value: &str, max_bytes: usize) -> bool {
    safe_text(value, max_bytes)
}

fn valid_capability_id(value: &str) -> bool {
    let mut segments = value.split('.');
    let Some(first) = segments.next() else {
        return false;
    };
    !first.is_empty()
        && first.as_bytes()[0].is_ascii_lowercase()
        && first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && segments.all(|segment| {
            !segment.is_empty()
                && segment.as_bytes()[0].is_ascii_lowercase()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
}

fn validate_identity(identity: &HelpIdentity) -> Result<(), HelpParseError> {
    if !valid_digest(&identity.corpus_digest)
        || !valid_digest(&identity.source_digest)
        || !valid_digest(&identity.model_digest)
        || !valid_id(&identity.model_id, 256)
        || !valid_id(&identity.model_version, 256)
    {
        return Err(error("invalid Help identity"));
    }
    Ok(())
}

fn validate_authorization(authorization: &HelpAuthorization) -> Result<(), HelpParseError> {
    if authorization.authorized_capabilities.len() > 64 {
        return Err(error("too many Help capabilities"));
    }
    let mut ids = HashSet::new();
    for capability in &authorization.authorized_capabilities {
        if !valid_id(capability, 128) || !valid_capability_id(capability) || !ids.insert(capability)
        {
            return Err(error("invalid Help capability set"));
        }
    }
    match authorization.mode {
        HelpAccessMode::Public if !authorization.authorized_capabilities.is_empty() => {
            Err(error("public Help authorization must be empty"))
        }
        HelpAccessMode::Authorized if authorization.authorized_capabilities.is_empty() => {
            Err(error("authorized Help access requires capabilities"))
        }
        _ => Ok(()),
    }
}

fn validate_provider(provider: &HelpProvider) -> Result<(), HelpParseError> {
    if !valid_id(&provider.profile, 256)
        || !valid_id(&provider.tenant, 256)
        || !valid_id(&provider.model, 256)
        || !valid_id(&provider.route_revision, 256)
    {
        return Err(error("invalid Help provider identity"));
    }
    Ok(())
}

fn validate_deadline(deadline: &HelpDeadline) -> Result<(), HelpParseError> {
    if !safe_text(&deadline.deadline_at, 64)
        || deadline.max_duration_ms == 0
        || deadline.max_duration_ms > HELP_AUTHORITY_MAX_DURATION_MS
    {
        return Err(error("invalid Help deadline"));
    }
    Ok(())
}

fn validate_context(
    context: &[HelpContextChunk],
    authorization: &HelpAuthorization,
) -> Result<(), HelpParseError> {
    if context.is_empty() || context.len() > HELP_AUTHORITY_MAX_CONTEXT_CHUNKS {
        return Err(error("invalid Help context count"));
    }
    let mut chunk_ids = HashSet::new();
    for chunk in context {
        if !valid_id(&chunk.chunk_id, 256)
            || !valid_id(&chunk.article_id, 256)
            || !chunk_ids.insert(&chunk.chunk_id)
            || !safe_text(&chunk.text, 512)
            || !valid_digest(&chunk.text_digest)
            || chunk.text_digest != sha256_digest(&chunk.text)
            || chunk.span_start != 0
            || chunk.span_end != chunk.text.len()
            || chunk.span_end == 0
        {
            return Err(error("invalid Help context chunk"));
        }
        if chunk.access == HelpAccess::Public {
            if !chunk.required_capabilities.is_empty() {
                return Err(error("public Help context carries capabilities"));
            }
        } else {
            if !matches!(authorization.mode, HelpAccessMode::Authorized)
                || chunk.required_capabilities.is_empty()
                || chunk
                    .required_capabilities
                    .iter()
                    .any(|capability| !authorization.authorized_capabilities.contains(capability))
            {
                return Err(error("Help context is not authorized"));
            }
        }
        if chunk.required_capabilities.len() > 16 {
            return Err(error("too many Help context capabilities"));
        }
        if chunk
            .required_capabilities
            .iter()
            .any(|capability| !valid_id(capability, 128) || !valid_capability_id(capability))
        {
            return Err(error("invalid Help context capability"));
        }
        let mut source_ids = HashSet::new();
        if chunk.source_bindings.is_empty()
            || chunk.source_bindings.len() > 8
            || chunk.source_bindings.iter().any(|source| {
                !valid_id(&source.source_id, 256)
                    || !valid_digest(&source.source_section_digest)
                    || source.source_byte_length == 0
                    || source.source_byte_length > 1_048_576
                    || !source_ids.insert(&source.source_id)
            })
        {
            return Err(error("invalid Help source bindings"));
        }
    }
    Ok(())
}

pub fn validate_help_request(request: &HelpAuthorityRequest) -> Result<(), HelpParseError> {
    if request.schema != HELP_AUTHORITY_SCHEMA
        || request.kind != HelpMessageKind::Request
        || !valid_id(&request.request_id, 256)
        || !safe_text(&request.query, 512)
        || !request.tools_disabled
        || !request.conversation_disabled
    {
        return Err(error("invalid Help request envelope"));
    }
    validate_authorization(&request.authorization)?;
    validate_identity(&request.identity)?;
    validate_provider(&request.provider)?;
    validate_deadline(&request.deadline)?;
    validate_context(&request.context, &request.authorization)?;
    let bytes =
        serde_json::to_vec(request).map_err(|_| error("Help request is not serializable"))?;
    if bytes.len() > HELP_AUTHORITY_MAX_REQUEST_BYTES {
        return Err(error("Help request exceeds byte limit"));
    }
    Ok(())
}

fn validate_cleanup(
    cleanup: &HelpCleanupReceipt,
    request_id: &str,
    require_finalized: bool,
) -> Result<(), HelpParseError> {
    if cleanup.schema != HELP_AUTHORITY_SCHEMA
        || cleanup.kind != HelpMessageKind::Cleanup
        || cleanup.request_id != request_id
        || cleanup.artifact_counts != HelpArtifactCounts::default()
    {
        return Err(error("invalid Help cleanup receipt"));
    }
    if require_finalized
        && (!matches!(cleanup.status, HelpCleanupStatus::Finalized)
            || !matches!(cleanup.provider_task, HelpProviderTask::Joined)
            || !matches!(cleanup.queue_slot, HelpQueueSlot::Released))
    {
        return Err(error("Help cleanup is uncertain"));
    }
    let bytes =
        serde_json::to_vec(cleanup).map_err(|_| error("Help cleanup is not serializable"))?;
    if bytes.len() > HELP_AUTHORITY_MAX_CLEANUP_BYTES {
        return Err(error("Help cleanup exceeds byte limit"));
    }
    Ok(())
}

fn claim_supported(claim: &HelpClaim, citations: &[HelpCitation]) -> bool {
    let quoted = citations
        .iter()
        .filter(|citation| claim.citation_ids.contains(&citation.citation_id))
        .map(|citation| citation.quoted_text.to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join(" ");
    let terms = claim
        .text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| term.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    !terms.is_empty() && terms.iter().all(|term| quoted.contains(term))
}

fn validate_response_fields(
    response: &HelpAuthorityResponse,
    request: &HelpAuthorityRequest,
) -> Result<(), HelpParseError> {
    if response.schema != HELP_AUTHORITY_SCHEMA
        || response.kind != HelpMessageKind::Response
        || response.request_id != request.request_id
        || response.identity != request.identity
        || response.provider != request.provider
        || response.deadline != request.deadline
        || !safe_text(&response.answer, 4_096)
        || !safe_text(&response.uncertainty, 1_024)
        || response.claims.is_empty()
        || response.claims.len() > HELP_AUTHORITY_MAX_CLAIMS
        || response.citations.is_empty()
        || response.citations.len() > HELP_AUTHORITY_MAX_CITATIONS
    {
        return Err(error("invalid Help response envelope"));
    }
    let contexts = request
        .context
        .iter()
        .map(|chunk| (chunk.chunk_id.as_str(), chunk))
        .collect::<std::collections::HashMap<_, _>>();
    let mut citation_ids = HashSet::new();
    for citation in &response.citations {
        let Some(context) = contexts.get(citation.chunk_id.as_str()) else {
            return Err(error("Help citation is outside context"));
        };
        if !valid_id(&citation.citation_id, 256)
            || !citation_ids.insert(&citation.citation_id)
            || citation.article_id != context.article_id
            || citation.span_start >= citation.span_end
            || citation.span_end > context.text.len()
            || context.text.get(citation.span_start..citation.span_end)
                != Some(citation.quoted_text.as_str())
            || !safe_text(&citation.quoted_text, 512)
            || !valid_digest(&citation.quoted_text_hash)
            || citation.quoted_text_hash != sha256_digest(&citation.quoted_text)
            || !valid_id(&citation.source_id, 256)
            || !valid_digest(&citation.source_section_digest)
            || citation.claim_ids.is_empty()
            || citation.claim_ids.len() > HELP_AUTHORITY_MAX_CLAIMS
            || !context.source_bindings.iter().any(|binding| {
                binding.source_id == citation.source_id
                    && binding.source_section_digest == citation.source_section_digest
            })
        {
            return Err(error("invalid Help citation"));
        }
    }
    let mut claim_ids = HashSet::new();
    let mut cursor = 0;
    for claim in &response.claims {
        if !valid_id(&claim.claim_id, 256)
            || !claim_ids.insert(&claim.claim_id)
            || claim.span_start >= claim.span_end
            || claim.span_end > response.answer.len()
            || response.answer.get(claim.span_start..claim.span_end) != Some(claim.text.as_str())
            || !safe_text(&claim.text, 1_024)
            || claim.citation_ids.is_empty()
            || claim.citation_ids.len() > HELP_AUTHORITY_MAX_CITATIONS
            || claim
                .citation_ids
                .iter()
                .any(|id| !citation_ids.contains(id))
        {
            return Err(error("unsupported Help claim"));
        }
        if response
            .answer
            .get(cursor..claim.span_start)
            .is_some_and(|gap| !gap.trim().is_empty())
        {
            return Err(error("Help answer has uncited text"));
        }
        if !claim_supported(claim, &response.citations) {
            return Err(error("Help claim is not supported by quoted text"));
        }
        cursor = claim.span_end;
    }
    if response
        .answer
        .get(cursor..)
        .is_some_and(|tail| !tail.trim().is_empty())
    {
        return Err(error("Help answer has uncited trailing text"));
    }
    for citation in &response.citations {
        if citation
            .claim_ids
            .iter()
            .any(|claim_id| !claim_ids.contains(claim_id))
        {
            return Err(error("Help citation names an unknown claim"));
        }
    }
    for claim in &response.claims {
        if claim.citation_ids.iter().any(|citation_id| {
            response
                .citations
                .iter()
                .find(|citation| citation.citation_id == *citation_id)
                .is_none_or(|citation| !citation.claim_ids.contains(&claim.claim_id))
        }) {
            return Err(error("Help claim/citation mapping is not bidirectional"));
        }
    }
    validate_cleanup(&response.cleanup, &request.request_id, true)?;
    Ok(())
}

pub fn validate_help_response(
    response: &HelpAuthorityResponse,
    request: &HelpAuthorityRequest,
) -> Result<(), HelpParseError> {
    validate_help_request(request)?;
    validate_response_fields(response, request)?;
    let bytes =
        serde_json::to_vec(response).map_err(|_| error("Help response is not serializable"))?;
    if bytes.len() > HELP_AUTHORITY_MAX_RESPONSE_BYTES {
        return Err(error("Help response exceeds byte limit"));
    }
    Ok(())
}

pub fn validate_help_cleanup(cleanup: &HelpCleanupReceipt) -> Result<(), HelpParseError> {
    validate_cleanup(cleanup, &cleanup.request_id, false)
}

pub fn parse_help_request(bytes: &[u8]) -> Result<HelpAuthorityRequest, HelpParseError> {
    if bytes.len() > HELP_AUTHORITY_MAX_REQUEST_BYTES {
        return Err(error("Help request exceeds byte limit"));
    }
    let request =
        serde_json::from_slice(bytes).map_err(|_| error("Help request JSON is invalid"))?;
    validate_help_request(&request)?;
    Ok(request)
}

pub fn parse_help_response(
    bytes: &[u8],
    request: &HelpAuthorityRequest,
) -> Result<HelpAuthorityResponse, HelpParseError> {
    if bytes.len() > HELP_AUTHORITY_MAX_RESPONSE_BYTES {
        return Err(error("Help response exceeds byte limit"));
    }
    let response =
        serde_json::from_slice(bytes).map_err(|_| error("Help response JSON is invalid"))?;
    validate_help_response(&response, request)?;
    Ok(response)
}

pub fn parse_help_cleanup(bytes: &[u8]) -> Result<HelpCleanupReceipt, HelpParseError> {
    if bytes.len() > HELP_AUTHORITY_MAX_CLEANUP_BYTES {
        return Err(error("Help cleanup exceeds byte limit"));
    }
    let cleanup =
        serde_json::from_slice(bytes).map_err(|_| error("Help cleanup JSON is invalid"))?;
    validate_help_cleanup(&cleanup)?;
    Ok(cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> HelpAuthorityRequest {
        HelpAuthorityRequest {
            schema: HELP_AUTHORITY_SCHEMA.into(),
            kind: HelpMessageKind::Request,
            request_id: "request-1".into(),
            authorization: HelpAuthorization {
                mode: HelpAccessMode::Public,
                authorized_capabilities: vec![],
            },
            identity: HelpIdentity {
                corpus_digest:
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                source_digest:
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".into(),
                model_digest:
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc".into(),
                model_id: "offline-help".into(),
                model_version: "1".into(),
            },
            provider: HelpProvider {
                profile: "profile".into(),
                tenant: "tenant".into(),
                model: "model".into(),
                route_revision: "route-1".into(),
                dialect: HelpDialect::BrokerNative,
            },
            deadline: HelpDeadline {
                deadline_at: "2026-08-25T21:00:00Z".into(),
                max_duration_ms: 20_000,
            },
            query: "What is Help?".into(),
            context: vec![HelpContextChunk {
                chunk_id: "article#en.body.0".into(),
                article_id: "article".into(),
                access: HelpAccess::Public,
                required_capabilities: vec![],
                text: "Help answers cite source bytes.".into(),
                text_digest:
                    "sha256:a202decb78b381e5e0ccf96123deb430452f49f086ae58a54c98c37756e161bb".into(),
                span_start: 0,
                span_end: "Help answers cite source bytes.".len(),
                source_bindings: vec![HelpSourceBinding {
                    source_id: "source".into(),
                    source_section_digest:
                        "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"
                            .into(),
                    source_byte_length: 10,
                }],
            }],
            tools_disabled: true,
            conversation_disabled: true,
        }
    }

    #[test]
    fn nested_unknown_fields_fail_closed() {
        let value = serde_json::json!({
            "schema": HELP_AUTHORITY_SCHEMA,
            "kind": "request",
            "requestId": "request-1",
            "authorization": {"mode": "public", "authorizedCapabilities": [], "extra": true}
        });
        assert!(serde_json::from_value::<HelpAuthorityRequest>(value).is_err());
    }

    #[test]
    fn request_rejects_non_public_context_without_explicit_authority() {
        let mut value = request();
        value.context[0].access = HelpAccess::Gated;
        value.context[0].required_capabilities = vec!["run.review".into()];
        assert!(validate_help_request(&value).is_err());
    }

    #[test]
    fn request_round_trips_with_strict_dtos() {
        let value = request();
        validate_help_request(&value).expect("request is valid");
        let encoded = serde_json::to_vec(&value).expect("request serializes");
        assert_eq!(parse_help_request(&encoded).expect("request parses"), value);
    }
}
