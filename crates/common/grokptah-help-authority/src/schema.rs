//! JSON Schema for the Help authority contracts.
//!
//! Emitted from one place so the Rust structs, the checked-in schema document,
//! and the TypeScript mirror cannot drift apart independently. Every object is
//! `additionalProperties: false`, matching `deny_unknown_fields` on the Rust
//! side: a schema that permitted extra properties while the parser rejected
//! them would let a consumer build a request that validates and then fails.

use serde_json::{Value, json};

/// Schema document id.
pub const SCHEMA_ID: &str = "https://grokptah.dev/schemas/help-authority.v1.json";

fn visibility() -> Value {
    json!({ "enum": ["public", "project", "private"] })
}

fn capability() -> Value {
    json!({ "enum": ["help_search", "help_search_project", "help_search_private", "help_answer"] })
}

fn action() -> Value {
    json!({ "enum": ["search", "answer", "read_source"] })
}

fn deny_reason() -> Value {
    json!({
        "enum": [
            "unknown_schema",
            "missing_capability",
            "tenant_mismatch",
            "scope_mismatch",
            "malformed_scope",
            "stale_index",
            "bounds"
        ]
    })
}

fn digest() -> Value {
    json!({ "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" })
}

fn bounded_id() -> Value {
    json!({ "type": "string", "minLength": 1, "maxLength": crate::MAX_ID_BYTES })
}

/// The complete schema document: shared definitions plus both root messages.
#[must_use]
pub fn json_schema() -> Value {
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": SCHEMA_ID,
        "title": "GrokPtah Help authority v1",
        "oneOf": [
            { "$ref": "#/$defs/request" },
            { "$ref": "#/$defs/response" }
        ],
        "$defs": {
            "visibility": visibility(),
            "capability": capability(),
            "action": action(),
            "denyReason": deny_reason(),
            "digest": digest(),
            "boundedId": bounded_id(),
            "principal": {
                "type": "object",
                "additionalProperties": false,
                "required": ["principal_id", "tenant_id"],
                "properties": {
                    "principal_id": { "$ref": "#/$defs/boundedId" },
                    "tenant_id": { "$ref": "#/$defs/boundedId" },
                    "project_ids": { "type": "array", "items": { "$ref": "#/$defs/boundedId" } },
                    "capabilities": { "type": "array", "items": { "$ref": "#/$defs/capability" } }
                }
            },
            "sourceDescriptor": {
                "type": "object",
                "additionalProperties": false,
                "required": ["source_id", "visibility", "tenant_id", "digest"],
                "properties": {
                    "source_id": { "$ref": "#/$defs/boundedId" },
                    "visibility": { "$ref": "#/$defs/visibility" },
                    "tenant_id": { "$ref": "#/$defs/boundedId" },
                    "project_id": { "$ref": "#/$defs/boundedId" },
                    "owner_principal_id": { "$ref": "#/$defs/boundedId" },
                    "digest": { "$ref": "#/$defs/digest" }
                }
            },
            "sourceDecision": {
                "type": "object",
                "additionalProperties": false,
                "required": ["source_id", "allowed"],
                "properties": {
                    "source_id": { "$ref": "#/$defs/boundedId" },
                    "allowed": { "type": "boolean" },
                    "denied_because": { "$ref": "#/$defs/denyReason" }
                }
            },
            "decisionReceipt": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "schema", "action", "principal_id", "tenant_id",
                    "corpus_digest", "index_digest",
                    "allowed_source_ids", "denied", "receipt_digest"
                ],
                "properties": {
                    "schema": { "const": crate::HELP_DECISION_RESPONSE_SCHEMA },
                    "action": { "$ref": "#/$defs/action" },
                    "principal_id": { "$ref": "#/$defs/boundedId" },
                    "tenant_id": { "$ref": "#/$defs/boundedId" },
                    "corpus_digest": { "$ref": "#/$defs/digest" },
                    "index_digest": { "$ref": "#/$defs/digest" },
                    "allowed_source_ids": { "type": "array", "items": { "$ref": "#/$defs/boundedId" } },
                    "denied": { "type": "array", "items": { "$ref": "#/$defs/sourceDecision" } },
                    "receipt_digest": { "$ref": "#/$defs/digest" }
                }
            },
            "request": {
                "type": "object",
                "additionalProperties": false,
                "required": ["schema", "action", "principal", "corpus_digest", "index_digest"],
                "properties": {
                    "schema": { "const": crate::HELP_DECISION_REQUEST_SCHEMA },
                    "action": { "$ref": "#/$defs/action" },
                    "principal": { "$ref": "#/$defs/principal" },
                    "corpus_digest": { "$ref": "#/$defs/digest" },
                    "index_digest": { "$ref": "#/$defs/digest" },
                    "sources": {
                        "type": "array",
                        "maxItems": crate::MAX_SOURCES_PER_DECISION,
                        "items": { "$ref": "#/$defs/sourceDescriptor" }
                    }
                }
            },
            "response": {
                "type": "object",
                "additionalProperties": false,
                "required": ["schema", "allowed", "receipt"],
                "properties": {
                    "schema": { "const": crate::HELP_DECISION_RESPONSE_SCHEMA },
                    "allowed": { "type": "boolean" },
                    "denied_because": { "$ref": "#/$defs/denyReason" },
                    "receipt": { "$ref": "#/$defs/decisionReceipt" }
                }
            }
        }
    })
}

/// The schema as pretty-printed JSON.
///
/// Key order is NOT stable across build configurations: other crates in this
/// workspace enable `serde_json/preserve_order`, so a whole-workspace build
/// yields insertion order while a `-p` build of this crate alone yields sorted
/// order. The checked-in schema document is therefore compared by *parsed
/// equality*, never byte-for-byte.
#[must_use]
pub fn json_schema_string() -> String {
    let mut text = serde_json::to_string_pretty(&json_schema()).expect("schema serializes");
    text.push('\n');
    text
}
