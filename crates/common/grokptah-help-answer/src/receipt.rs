//! Typed, zero-artifact receipts.
//!
//! A receipt records that an answer was executed, under which admission, with
//! what result. It carries no artifact of the exchange at all: not the
//! question, not the answer, not a quote, not a path, not a heading, not a
//! provider message. Only ids, digests, counts, and timings.
//!
//! That is the point. An answer contract that refuses to persist anything, and
//! then writes the question and the answer into an audit log, has not refused
//! to persist anything — it has moved where the copy lives.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Wire schema id for an execution receipt.
pub const HELP_ANSWER_RECEIPT_SCHEMA: &str = "grokptah.help-answer-receipt.v1";

/// How one execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// The provider replied and the reply held the contract.
    Answered,
    /// The provider replied and the reply was refused.
    Rejected,
    /// Refused before any provider was reached.
    Denied,
    /// Cancelled, and the worker stopped.
    Cancelled,
    /// The deadline passed, and the worker stopped.
    TimedOut,
    /// Cancelled or expired, and the worker did **not** stop within the join
    /// budget. The slot it holds is still held; see
    /// [`ExecutorStats::stuck`](crate::ExecutorStats::stuck).
    Abandoned,
    /// The provider itself failed.
    ProviderError,
    /// Never admitted to the queue.
    Refused,
}

impl ExecutionOutcome {
    /// Short, stable label used in the receipt digest.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Answered => "answered",
            Self::Rejected => "rejected",
            Self::Denied => "denied",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Abandoned => "abandoned",
            Self::ProviderError => "provider_error",
            Self::Refused => "refused",
        }
    }
}

/// Why an execution did not produce an answer.
///
/// Coarse on purpose, and never the provider's own message: a provider error
/// string can carry a URL, a header, or a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureReason {
    /// The admission did not verify against what is being served.
    AdmissionRefused,
    /// The request was not the bounded, tool-free shape.
    RequestRefused,
    /// The reply was not shaped like a reply to this request.
    ReplyRefused,
    /// The queue was at its bound.
    QueueFull,
    /// The executor was shutting down.
    ShuttingDown,
    /// The caller cancelled.
    CallerCancelled,
    /// The deadline passed.
    DeadlineExceeded,
    /// The provider returned an error.
    ProviderFailed,
}

impl FailureReason {
    /// Short, stable label used in the receipt digest.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::AdmissionRefused => "admission_refused",
            Self::RequestRefused => "request_refused",
            Self::ReplyRefused => "reply_refused",
            Self::QueueFull => "queue_full",
            Self::ShuttingDown => "shutting_down",
            Self::CallerCancelled => "caller_cancelled",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::ProviderFailed => "provider_failed",
        }
    }
}

/// One execution, recorded without any artifact of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ExecutionReceipt {
    /// Wire schema id.
    pub schema: String,
    /// Admission this execution ran under.
    pub admission_id: String,
    /// Request body the admission was minted for.
    pub request_digest: String,
    /// Corpus served at dispatch.
    pub corpus_digest: String,
    /// Index served at dispatch.
    pub index_digest: String,
    /// Manifest served at dispatch.
    pub manifest_digest: String,
    /// Grant revision the admission was minted under.
    pub grant_revision: u64,
    /// How it ended.
    pub outcome: ExecutionOutcome,
    /// Why, when it did not answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureReason>,
    /// Outcome binding, present only for an answer that held the contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_digest: Option<String>,
    /// Source anchor ids the accepted answer cited, sorted and deduplicated.
    pub cited_source_ids: Vec<String>,
    /// How many claims the accepted answer was segmented into.
    pub claim_count: u32,
    /// Milliseconds spent waiting for a worker.
    pub queued_ms: u64,
    /// Milliseconds spent inside the provider call.
    pub ran_ms: u64,
    /// Digest over every field above, for audit correlation.
    pub receipt_digest: String,
}

fn domain_digest(domain: &str, fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for field in std::iter::once(&domain).chain(fields.iter()) {
        hasher.update(field.len().to_string().as_bytes());
        hasher.update(b":");
        hasher.update(field.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Inputs a receipt is built from.
#[derive(Debug, Clone)]
pub struct ReceiptInputs {
    /// Admission this execution ran under.
    pub admission_id: String,
    /// Request body the admission was minted for.
    pub request_digest: String,
    /// Corpus served at dispatch.
    pub corpus_digest: String,
    /// Index served at dispatch.
    pub index_digest: String,
    /// Manifest served at dispatch.
    pub manifest_digest: String,
    /// Grant revision the admission was minted under.
    pub grant_revision: u64,
    /// How it ended.
    pub outcome: ExecutionOutcome,
    /// Why, when it did not answer.
    pub failure: Option<FailureReason>,
    /// Outcome binding, for an accepted answer.
    pub outcome_digest: Option<String>,
    /// Source anchor ids cited.
    pub cited_source_ids: Vec<String>,
    /// Claims the accepted answer was segmented into.
    pub claim_count: u32,
    /// Milliseconds waiting for a worker.
    pub queued_ms: u64,
    /// Milliseconds inside the provider call.
    pub ran_ms: u64,
}

/// Build a receipt and seal it with a digest over its own fields.
///
/// Every variable-length part is preceded by its count, and the outcome and
/// failure are digested by stable label rather than by `Debug` formatting, so
/// a rename in the source cannot silently change historical digests.
#[must_use]
pub fn build_receipt(inputs: ReceiptInputs) -> ExecutionReceipt {
    let mut cited = inputs.cited_source_ids;
    cited.sort();
    cited.dedup();

    let mut fields: Vec<String> = vec![
        HELP_ANSWER_RECEIPT_SCHEMA.to_string(),
        inputs.admission_id.clone(),
        inputs.request_digest.clone(),
        inputs.corpus_digest.clone(),
        inputs.index_digest.clone(),
        inputs.manifest_digest.clone(),
        inputs.grant_revision.to_string(),
        inputs.outcome.label().to_string(),
        inputs
            .failure
            .map_or_else(|| "none".to_string(), |reason| reason.label().to_string()),
        inputs
            .outcome_digest
            .clone()
            .unwrap_or_else(|| "none".to_string()),
        inputs.claim_count.to_string(),
        inputs.queued_ms.to_string(),
        inputs.ran_ms.to_string(),
        cited.len().to_string(),
    ];
    fields.extend(cited.iter().cloned());
    let refs: Vec<&str> = fields.iter().map(String::as_str).collect();

    ExecutionReceipt {
        schema: HELP_ANSWER_RECEIPT_SCHEMA.to_string(),
        admission_id: inputs.admission_id,
        request_digest: inputs.request_digest,
        corpus_digest: inputs.corpus_digest,
        index_digest: inputs.index_digest,
        manifest_digest: inputs.manifest_digest,
        grant_revision: inputs.grant_revision,
        outcome: inputs.outcome,
        failure: inputs.failure,
        outcome_digest: inputs.outcome_digest,
        cited_source_ids: cited,
        claim_count: inputs.claim_count,
        queued_ms: inputs.queued_ms,
        ran_ms: inputs.ran_ms,
        receipt_digest: domain_digest("grokptah.help.answer-receipt.v1", &refs),
    }
}
