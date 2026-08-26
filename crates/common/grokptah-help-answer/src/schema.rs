//! JSON Schema peer for the answer contracts.
//!
//! Emitted from this crate so a consumer validates against exactly the
//! document this build enforces, rather than a copy that has drifted from it.
//! `additionalProperties: false` everywhere mirrors `deny_unknown_fields`: a
//! field one side would drop is a field the other side must reject.

use serde_json::{Value, json};

/// Schema id for the answer contract document.
pub const HELP_ANSWER_SCHEMA_ID: &str = "grokptah.help-answer.v1";

fn string() -> Value {
    json!({ "type": "string" })
}

fn object(properties: Value, required: Vec<&str>) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

/// The JSON Schema for requests, replies, and receipts.
#[must_use]
pub fn json_schema() -> Value {
    let context_chunk = object(
        json!({
            "chunkId": string(),
            "articleId": string(),
            "chunkDigest": string(),
            "text": string(),
            "sourceIds": { "type": "array", "items": string() },
        }),
        vec!["chunkId", "articleId", "chunkDigest", "text", "sourceIds"],
    );

    let citation = object(
        json!({
            "claimIndex": { "type": "integer", "minimum": 0 },
            "chunkId": string(),
            "articleId": string(),
            "sourceId": string(),
            "quote": string(),
        }),
        vec!["claimIndex", "chunkId", "articleId", "sourceId", "quote"],
    );

    let request = object(
        json!({
            "schema": { "const": crate::HELP_ANSWER_REQUEST_SCHEMA },
            "query": { "type": "string", "maxLength": crate::MAX_QUERY_CHARS },
            "corpusDigest": string(),
            "indexDigest": string(),
            "context": {
                "type": "array",
                "maxItems": crate::MAX_CONTEXT_CHUNKS,
                "items": context_chunk,
            },
            "instruction": string(),
            // Not booleans: the only accepted value is the one that makes this
            // the bounded contract rather than an ordinary chat turn.
            "toolsDisabled": { "const": true },
            "conversationDisabled": { "const": true },
            "maxAnswerChars": { "type": "integer", "minimum": 1 },
        }),
        vec![
            "schema",
            "query",
            "corpusDigest",
            "indexDigest",
            "context",
            "instruction",
            "toolsDisabled",
            "conversationDisabled",
            "maxAnswerChars",
        ],
    );

    let reply = object(
        json!({
            "schema": { "const": crate::HELP_ANSWER_RESPONSE_SCHEMA },
            "answer": string(),
            "citations": {
                "type": "array",
                "minItems": 1,
                "maxItems": crate::MAX_CITATIONS,
                "items": citation,
            },
            "uncertainty": string(),
            "corpusDigest": string(),
            "admissionId": string(),
        }),
        vec![
            "schema",
            "answer",
            "citations",
            "uncertainty",
            "corpusDigest",
            "admissionId",
        ],
    );

    let receipt = object(
        json!({
            "schema": { "const": crate::HELP_ANSWER_RECEIPT_SCHEMA },
            "admissionId": string(),
            "requestDigest": string(),
            "corpusDigest": string(),
            "indexDigest": string(),
            "manifestDigest": string(),
            "grantRevision": { "type": "integer", "minimum": 0 },
            "outcome": { "enum": [
                "answered", "rejected", "denied", "cancelled",
                "timed_out", "abandoned", "provider_error", "refused",
            ] },
            "failure": { "enum": [
                "admission_refused", "request_refused", "reply_refused", "queue_full",
                "shutting_down", "caller_cancelled", "deadline_exceeded", "provider_failed",
            ] },
            "outcomeDigest": string(),
            "citedSourceIds": { "type": "array", "items": string() },
            "claimCount": { "type": "integer", "minimum": 0 },
            "queuedMs": { "type": "integer", "minimum": 0 },
            "ranMs": { "type": "integer", "minimum": 0 },
            "receiptDigest": string(),
        }),
        vec![
            "schema",
            "admissionId",
            "requestDigest",
            "corpusDigest",
            "indexDigest",
            "manifestDigest",
            "grantRevision",
            "outcome",
            "citedSourceIds",
            "claimCount",
            "queuedMs",
            "ranMs",
            "receiptDigest",
        ],
    );

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": HELP_ANSWER_SCHEMA_ID,
        "title": "GrokPtah Help answer contracts",
        "$defs": {
            "answerRequest": request,
            "answerReply": reply,
            "executionReceipt": receipt,
        },
    })
}
