//! Closed answer contracts, and the request digest an admission is minted over.
//!
//! Every type is `deny_unknown_fields`. A provider reply carrying a field this
//! version does not know about is refused rather than having it dropped: a
//! silently ignored `claimIndex` would turn claim-bound coverage back into the
//! aggregate ratio it replaced.
//!
//! [`request_digest`] is byte-identical to the TypeScript
//! `helpAnswerRequestDigest`, and a shared fixture proves it. That matters
//! because the admission binds this value: if the two sides disagreed about
//! how to digest a request, every admission would fail to verify — or, worse,
//! one that should not verify would.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Wire schema id for a request.
pub const HELP_ANSWER_REQUEST_SCHEMA: &str = "grokptah.help-answer-request.v1";
/// Wire schema id for a provider reply.
pub const HELP_ANSWER_RESPONSE_SCHEMA: &str = "grokptah.help-answer-response.v1";

/// Longest query this contract will carry, in characters.
pub const MAX_QUERY_CHARS: usize = 512;
/// Most context chunks one request may carry.
pub const MAX_CONTEXT_CHUNKS: usize = 8;
/// Most citations one reply may carry.
pub const MAX_CITATIONS: usize = 16;

/// One retrieved chunk offered to the provider as context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerContextChunk {
    /// Stable, citable chunk id.
    pub chunk_id: String,
    /// Article the chunk belongs to.
    pub article_id: String,
    /// Digest of the chunk's own text, so a rebuilt corpus is detectable.
    pub chunk_digest: String,
    /// The chunk text, already sanitized and bounded.
    pub text: String,
    /// Source anchor ids backing this chunk.
    pub source_ids: Vec<String>,
}

/// The request body, before an admission is attached.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerRequestCore {
    /// Wire schema id.
    pub schema: String,
    /// The bounded, redacted question.
    pub query: String,
    /// Corpus the context was drawn from.
    pub corpus_digest: String,
    /// Index the context was retrieved with.
    pub index_digest: String,
    /// Selected context. Never empty in practice; an empty list still digests.
    pub context: Vec<AnswerContextChunk>,
    /// The fixed instruction. Not caller-chosen at dispatch time.
    pub instruction: String,
    /// Always true: this contract never carries tool definitions.
    pub tools_disabled: bool,
    /// Always true: nothing from this exchange is written to a conversation.
    pub conversation_disabled: bool,
    /// Ceiling the provider is told to write within.
    pub max_answer_chars: u32,
}

/// One citation as an untrusted provider supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerCitationInput {
    /// Zero-based index of the answer claim this citation is evidence for.
    pub claim_index: u32,
    /// Chunk the quote is from.
    pub chunk_id: String,
    /// Article the chunk belongs to.
    pub article_id: String,
    /// Source anchor backing the chunk.
    pub source_id: String,
    /// Verbatim text from the chunk.
    pub quote: String,
}

/// An untrusted provider reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AnswerReply {
    /// Wire schema id.
    pub schema: String,
    /// The phrased answer.
    pub answer: String,
    /// Claim-bound citations.
    pub citations: Vec<AnswerCitationInput>,
    /// What the provider is unsure about.
    pub uncertainty: String,
    /// Corpus the provider believes it answered over.
    pub corpus_digest: String,
    /// Admission the reply claims to be against.
    pub admission_id: String,
}

/// Domain-separated, length-prefixed digest over a field list.
///
/// Duplicated from the authority crate rather than re-exported: this is a wire
/// format, and a shared private helper changing shape would silently change
/// every digest on both sides at once.
fn domain_digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in std::iter::once(&domain).chain(fields.iter()) {
        hasher.update(field.len().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Digest the request body an admission is minted over.
///
/// Byte-identical to the TypeScript `helpAnswerRequestDigest`. Source ids are
/// sorted so two identical requests digest identically regardless of the order
/// retrieval happened to emit them in, and every variable-length list is
/// preceded by its own count so no rearrangement can forge a match.
#[must_use]
pub fn request_digest(core: &AnswerRequestCore) -> String {
    let mut fields: Vec<String> = vec![
        core.schema.clone(),
        core.query.clone(),
        core.corpus_digest.clone(),
        core.index_digest.clone(),
        core.instruction.clone(),
        core.tools_disabled.to_string(),
        core.conversation_disabled.to_string(),
        core.max_answer_chars.to_string(),
        core.context.len().to_string(),
    ];
    for chunk in &core.context {
        fields.push(chunk.chunk_id.clone());
        fields.push(chunk.article_id.clone());
        fields.push(chunk.chunk_digest.clone());
        fields.push(chunk.text.clone());
        fields.push(chunk.source_ids.len().to_string());
        let mut sorted = chunk.source_ids.clone();
        sorted.sort();
        fields.extend(sorted);
    }
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();
    domain_digest("grokptah.help.answer-request.v1", &refs)
}

/// Why a request or reply was refused before any provider was reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContractError {
    /// The wire schema was not the expected one.
    UnknownSchema(String),
    /// A tool or conversation flag was not the fixed value.
    NotBounded(&'static str),
    /// A bound was exceeded.
    Bounds(&'static str),
    /// The payload was not well-formed.
    Malformed(String),
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownSchema(found) => write!(formatter, "unknown schema: {found}"),
            Self::NotBounded(field) => write!(formatter, "contract flag not held: {field}"),
            Self::Bounds(field) => write!(formatter, "field out of bounds: {field}"),
            Self::Malformed(detail) => write!(formatter, "malformed payload: {detail}"),
        }
    }
}

impl std::error::Error for ContractError {}

impl AnswerRequestCore {
    /// Confirm the request is the bounded, tool-free shape this crate sends.
    ///
    /// # Errors
    /// Returns the first [`ContractError`] that applies.
    pub fn enforce(&self) -> Result<(), ContractError> {
        if self.schema != HELP_ANSWER_REQUEST_SCHEMA {
            return Err(ContractError::UnknownSchema(self.schema.clone()));
        }
        // Not defaults, not caller-chosen: a request that does not disable
        // tools and conversation is not this contract, whatever it claims.
        if !self.tools_disabled {
            return Err(ContractError::NotBounded("toolsDisabled"));
        }
        if !self.conversation_disabled {
            return Err(ContractError::NotBounded("conversationDisabled"));
        }
        if self.query.chars().count() > MAX_QUERY_CHARS {
            return Err(ContractError::Bounds("query"));
        }
        if self.context.len() > MAX_CONTEXT_CHUNKS {
            return Err(ContractError::Bounds("context"));
        }
        Ok(())
    }
}

impl AnswerReply {
    /// Confirm the reply is shaped like a reply to *this* request.
    ///
    /// Deliberately shallow: quote verification, claim coverage, and span
    /// binding are decided against the corpus, which this crate does not hold.
    /// What is checked here is what can be checked without it.
    ///
    /// # Errors
    /// Returns the first [`ContractError`] that applies.
    pub fn enforce(
        &self,
        core: &AnswerRequestCore,
        admission_id: &str,
    ) -> Result<(), ContractError> {
        if self.schema != HELP_ANSWER_RESPONSE_SCHEMA {
            return Err(ContractError::UnknownSchema(self.schema.clone()));
        }
        if self.admission_id != admission_id {
            return Err(ContractError::Malformed(
                "reply names another admission".into(),
            ));
        }
        if self.corpus_digest != core.corpus_digest {
            return Err(ContractError::Malformed(
                "reply names another corpus".into(),
            ));
        }
        if self.answer.trim().is_empty() {
            return Err(ContractError::Bounds("answer"));
        }
        if self.answer.chars().count() > core.max_answer_chars as usize {
            return Err(ContractError::Bounds("answer"));
        }
        if self.uncertainty.trim().is_empty() {
            return Err(ContractError::Bounds("uncertainty"));
        }
        if self.citations.is_empty() || self.citations.len() > MAX_CITATIONS {
            return Err(ContractError::Bounds("citations"));
        }
        let known: std::collections::BTreeSet<&str> = core
            .context
            .iter()
            .map(|chunk| chunk.chunk_id.as_str())
            .collect();
        for citation in &self.citations {
            if !known.contains(citation.chunk_id.as_str()) {
                return Err(ContractError::Malformed(
                    "citation outside the context".into(),
                ));
            }
        }
        Ok(())
    }
}
