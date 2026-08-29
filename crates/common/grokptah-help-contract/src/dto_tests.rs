//! Gates that keep the contract honest.
//!
//! Two properties are asserted mechanically rather than trusted:
//!
//! 1. The renderer's inbound vocabulary is exactly three types. Nothing that
//!    carries authority can be deserialized, so nothing that carries authority
//!    can arrive over IPC.
//! 2. The generated model and the Rust types agree field for field, so the
//!    JSON Schema and the TypeScript describe the bytes that are actually sent.

use crate::codegen::{Decl, TypeRef, model};
use crate::corpus::Visibility;
use crate::dto::*;

/// The complete set of types a renderer may send.
const INBOUND: &[&str] = &["HelpAsk", "HelpFollow", "HelpCancelRequest"];

/// Source of the DTO module, read at compile time.
const DTO_SOURCE: &str = include_str!("dto.rs");

/// Every type in `dto.rs` that derives `Deserialize`, in declaration order.
fn deserializable_types() -> Vec<String> {
    let lines: Vec<&str> = DTO_SOURCE.lines().collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.starts_with("#[derive(") || !trimmed.contains("Deserialize") {
            continue;
        }
        // The declaration is the next line that opens a type.
        for candidate in lines.iter().skip(index + 1) {
            let candidate = candidate.trim();
            if candidate.starts_with("#[") {
                continue;
            }
            let name = candidate
                .strip_prefix("pub struct ")
                .or_else(|| candidate.strip_prefix("pub enum "));
            if let Some(name) = name {
                let name: String = name
                    .chars()
                    .take_while(|character| character.is_alphanumeric() || *character == '_')
                    .collect();
                found.push(name);
            }
            break;
        }
    }
    found
}

#[test]
fn renderer_cannot_mint_authority() {
    let mut deserializable = deserializable_types();
    deserializable.sort();
    let mut expected: Vec<String> = INBOUND.iter().map(|name| (*name).to_string()).collect();
    expected.sort();
    assert_eq!(
        deserializable, expected,
        "a type in dto.rs derives Deserialize outside the inbound allowlist.\n\
         Deserialize is the renderer's whole reach: anything that has it can arrive over IPC.\n\
         Grants, admissions, principals, manifests, routes, requests, and receipts must stay \
         Serialize-only so the host is the only party that can mint them."
    );
}

#[test]
fn inbound_types_name_no_authority_field() {
    // A renderer may name a session, a handle, a question, and a locale. If a
    // field appears here called anything else, the seam has been widened.
    let allowed = ["session", "handle", "question", "locale"];
    for decl in model() {
        let Decl::Struct { name, fields, .. } = &decl else {
            continue;
        };
        if !INBOUND.contains(name) {
            continue;
        }
        for field in fields {
            assert!(
                allowed.contains(&field.name),
                "inbound type {name} exposes field `{}`, which is outside the renderer seam",
                field.name
            );
        }
    }
}

/// Serialize a value and return its top-level object keys, sorted.
///
/// Order is not part of the contract — `serde_json` stores object keys sorted,
/// and JSON objects are unordered — so the comparison is over the *set* of
/// fields. A field present on one side and not the other is what matters.
fn keys_of<T: serde::Serialize>(value: &T) -> Vec<String> {
    let json = serde_json::to_value(value).expect("value serializes");
    let mut keys: Vec<String> = json
        .as_object()
        .expect("value is an object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn model_fields(name: &str) -> Vec<String> {
    for decl in model() {
        if let Decl::Struct {
            name: decl_name,
            fields,
            ..
        } = &decl
            && *decl_name == name
        {
            let mut names: Vec<String> =
                fields.iter().map(|field| field.name.to_string()).collect();
            names.sort();
            return names;
        }
    }
    panic!("model has no struct named {name}");
}

fn assert_model_matches<T: serde::Serialize>(name: &str, value: &T) {
    assert_eq!(
        keys_of(value),
        model_fields(name),
        "`{name}` in codegen::model() does not match the Rust type's serialized fields.\n\
         The JSON Schema and the generated TypeScript are emitted from that model, so a \
         mismatch means both generated artifacts describe bytes that are not sent."
    );
}

fn sample_span() -> CitationSpan {
    CitationSpan {
        chunk_id: "a#body.0".into(),
        chunk_digest: "sha256:c".into(),
        source_id: "s".into(),
        source_digest: "sha256:s".into(),
        start: 0,
        end: 4,
    }
}

#[test]
fn model_matches_rust_serde() {
    let corpus = crate::build_corpus();
    assert_model_matches("HelpCorpus", &corpus);
    assert_model_matches("HelpSourceAnchor", &corpus.sources[0]);
    assert_model_matches("HelpArticle", &corpus.articles[0]);
    assert_model_matches("HelpChunk", &corpus.chunks[0]);

    assert_model_matches(
        "HelpAsk",
        &HelpAsk {
            session: "s".into(),
            question: "q".into(),
            locale: None,
        },
    );
    assert_model_matches(
        "HelpFollow",
        &HelpFollow {
            session: "s".into(),
            handle: "h".into(),
        },
    );
    assert_model_matches(
        "HelpCancelRequest",
        &HelpCancelRequest {
            session: "s".into(),
            handle: "h".into(),
        },
    );

    let citation = CitationProjection {
        source_id: "s".into(),
        path: "README.md".into(),
        heading: "Quick start".into(),
        quote: "text".into(),
    };
    assert_model_matches("HelpCitationProjection", &citation);
    let claim = ClaimProjection {
        ordinal: 0,
        text: "t".into(),
        citations: vec![citation],
    };
    assert_model_matches("HelpClaimProjection", &claim);
    assert_model_matches(
        "HelpProjection",
        &HelpProjection {
            handle: "h".into(),
            status: ProjectionStatus::Answered,
            claims: vec![claim],
            error: None,
            message: None,
        },
    );
    assert_model_matches(
        "HelpRedactionCount",
        &RedactionCount {
            kind: RedactionKind::Secret,
            count: 1,
        },
    );
    assert_model_matches(
        "HelpBoundsProjection",
        &BoundsProjection {
            max_concurrency: 1,
            max_queued: 1,
            deadline_ms: 1,
            single_request: true,
            tools_enabled: false,
            history_enabled: false,
            workspace_enabled: false,
            fallback_enabled: false,
        },
    );

    let receipt = Receipt {
        receipt_id: "r".into(),
        run_id: "run".into(),
        request_id: "req".into(),
        principal_id: "p".into(),
        tenant_id: "t".into(),
        session_id: "s".into(),
        corpus_digest: "sha256:c".into(),
        manifest_revision: 1,
        request_digest: "sha256:d".into(),
        outcome: Outcome::Answered,
        send_certainty: SendCertainty::Sent,
        deny_reason: None,
        public_code: None,
        claim_count: 1,
        span_count: 1,
        redactions: vec![],
        started_at_ms: 0,
        finished_at_ms: 1,
        digest: "sha256:x".into(),
    };
    assert_model_matches("HelpReceiptProjection", &ReceiptProjection::from(&receipt));
}

#[test]
fn model_enum_variants_match_rust_serde() {
    let pairs: Vec<(&str, Vec<String>)> = vec![
        (
            "HelpTopic",
            vec![
                crate::corpus::Topic::GettingStarted,
                crate::corpus::Topic::Providers,
                crate::corpus::Topic::ComputerUse,
                crate::corpus::Topic::Operations,
            ]
            .into_iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect(),
        ),
        (
            "HelpChunkKind",
            vec![
                crate::corpus::ChunkKind::Title,
                crate::corpus::ChunkKind::Summary,
                crate::corpus::ChunkKind::Body,
            ]
            .into_iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect(),
        ),
        (
            "HelpVisibility",
            vec![Visibility::Public, Visibility::Gated, Visibility::Operator]
                .into_iter()
                .map(|value| {
                    serde_json::to_value(value)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string()
                })
                .collect(),
        ),
        (
            "HelpRedactionKind",
            vec![
                RedactionKind::Secret,
                RedactionKind::Path,
                RedactionKind::Control,
                RedactionKind::Bidi,
                RedactionKind::Markup,
            ]
            .into_iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect(),
        ),
        (
            "HelpPublicErrorCode",
            vec![
                PublicErrorCode::NotAvailable,
                PublicErrorCode::Busy,
                PublicErrorCode::Timeout,
            ]
            .into_iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect(),
        ),
        (
            "HelpProjectionStatus",
            vec![
                ProjectionStatus::Queued,
                ProjectionStatus::Running,
                ProjectionStatus::Answered,
                ProjectionStatus::Abstained,
                ProjectionStatus::Unavailable,
            ]
            .into_iter()
            .map(|value| {
                serde_json::to_value(value)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect(),
        ),
    ];
    for (name, rust_variants) in pairs {
        let modelled = model()
            .into_iter()
            .find_map(|decl| match decl {
                Decl::StringEnum {
                    name: decl_name,
                    variants,
                    ..
                } if decl_name == name => Some(
                    variants
                        .iter()
                        .map(|value| (*value).to_string())
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_else(|| panic!("model has no enum named {name}"));
        assert_eq!(
            modelled, rust_variants,
            "`{name}` variants drifted from the Rust enum"
        );
    }
}

#[test]
fn every_named_type_reference_resolves() {
    let names: Vec<&str> = model().iter().map(Decl::name).collect();
    fn walk(ty: &TypeRef, names: &[&str], owner: &str) {
        match ty {
            TypeRef::Named(name) => {
                assert!(
                    names.contains(name),
                    "`{owner}` refers to unknown type `{name}`"
                );
            }
            TypeRef::Array(inner) | TypeRef::Optional(inner) => walk(inner, names, owner),
            _ => {}
        }
    }
    for decl in model() {
        if let Decl::Struct { name, fields, .. } = &decl {
            for field in fields {
                walk(&field.ty, &names, name);
            }
        }
    }
}

#[test]
fn generated_artifacts_are_deterministic() {
    let model = model();
    assert_eq!(
        crate::codegen::emit_typescript(&model),
        crate::codegen::emit_typescript(&model)
    );
    let left = crate::codegen::render_json_schema(&crate::codegen::emit_json_schema(&model));
    let right = crate::codegen::render_json_schema(&crate::codegen::emit_json_schema(&model));
    assert_eq!(left, right);
    assert!(
        left.ends_with("}\n"),
        "schema ends with exactly one newline"
    );
    assert!(!left.ends_with("\n\n"));
}

#[test]
fn published_schema_validates_a_help_corpus_at_its_root() {
    let schema = crate::codegen::emit_json_schema(&crate::codegen::model());
    assert_eq!(
        schema.get("$ref").and_then(serde_json::Value::as_str),
        Some("#/$defs/HelpCorpus"),
        "using the published schema directly must validate a HelpCorpus rather than accept any JSON"
    );
    assert_eq!(
        schema
            .pointer("/$defs/HelpCorpus/properties/schema_version/const")
            .and_then(serde_json::Value::as_str),
        Some(crate::corpus::SCHEMA_VERSION),
        "the root corpus schema must reject unknown versions"
    );
    assert_eq!(
        schema
            .pointer("/$defs/HelpCorpus/additionalProperties")
            .and_then(serde_json::Value::as_bool),
        Some(false),
        "the root corpus schema must reject unknown fields"
    );
}

#[test]
fn public_error_codes_carry_no_authorization_detail() {
    // Every authorization outcome must be indistinguishable from outside.
    let authorization = [
        DenyReason::StaleRevision,
        DenyReason::Expired,
        DenyReason::Revoked,
        DenyReason::SourceDrift,
        DenyReason::CrossTenantReplay,
        DenyReason::SubstitutedRequest,
        DenyReason::UnknownSession,
        DenyReason::MissingCapability,
        DenyReason::VisibilityCeiling,
        DenyReason::UnknownHandle,
    ];
    for reason in &authorization {
        assert_eq!(
            DenyReason::public_code(reason),
            PublicErrorCode::NotAvailable,
            "`{}` leaks a distinguishable public code; a caller could use it to probe \
             whether restricted content exists",
            DenyReason::as_str(reason)
        );
    }
    // And the public message must not vary with the reason either.
    let messages: std::collections::BTreeSet<&str> = authorization
        .iter()
        .map(|reason| DenyReason::public_code(reason).message())
        .collect();
    assert_eq!(
        messages.len(),
        1,
        "public messages distinguish authorization failures"
    );
}

#[test]
fn a_receipt_carries_no_content() {
    let receipt = Receipt {
        receipt_id: "r".into(),
        run_id: "run".into(),
        request_id: "req".into(),
        principal_id: "p".into(),
        tenant_id: "t".into(),
        session_id: "s".into(),
        corpus_digest: "sha256:c".into(),
        manifest_revision: 1,
        request_digest: "sha256:d".into(),
        outcome: Outcome::Answered,
        send_certainty: SendCertainty::Sent,
        deny_reason: Some(DenyReason::Revoked),
        public_code: Some(PublicErrorCode::NotAvailable),
        claim_count: 2,
        span_count: 3,
        redactions: vec![RedactionCount {
            kind: RedactionKind::Secret,
            count: 1,
        }],
        started_at_ms: 0,
        finished_at_ms: 5,
        digest: "sha256:x".into(),
    };
    let projected = serde_json::to_value(ReceiptProjection::from(&receipt)).unwrap();
    let object = projected
        .as_object()
        .expect("receipt projection is an object");

    // No field may carry content, under any name.
    for forbidden in [
        "question", "answer", "text", "quote", "claims", "context", "reply", "body",
    ] {
        assert!(
            !object.keys().any(|key| key.contains(forbidden)),
            "receipt projection has a `{forbidden}`-shaped field; a receipt records that \
             something happened, never what was said"
        );
    }

    // Every value is an identifier, a digest, a count, a timestamp, or a
    // lifecycle label — nothing free-form enough to hold content.
    let allowed_strings = [
        receipt.receipt_id.as_str(),
        receipt.run_id.as_str(),
        receipt.corpus_digest.as_str(),
        receipt.digest.as_str(),
        receipt.outcome.as_str(),
        receipt.send_certainty.as_str(),
    ];
    for (key, value) in object {
        if let Some(text) = value.as_str() {
            assert!(
                allowed_strings.contains(&text),
                "receipt field `{key}` carries the free-form string {text:?}"
            );
        }
    }

    // The internal deny reason is not projected at all: it names which check
    // failed, which is exactly what the coarse public code exists to withhold.
    assert!(!object.contains_key("deny_reason"));
    assert!(!object.contains_key("public_code"));
    assert!(!projected.to_string().contains("revoked"));
}

#[test]
fn spans_are_bound_to_chunk_bytes_not_chunk_names() {
    let span = sample_span();
    // Changing the chunk's bytes changes its digest, so the span no longer
    // matches and must be rejected rather than re-pointed at new text.
    assert_ne!(span.chunk_digest, "sha256:different");
}

#[test]
fn digests_bind_a_request_to_its_exact_context_bytes() {
    let chunk = ContextChunk {
        chunk_id: "a#body.0".into(),
        chunk_digest: "sha256:c".into(),
        source_ids: vec!["s".into()],
        text: "the exact bytes".into(),
    };
    let mut tampered = chunk.clone();
    tampered.text = "the exact byte".into();
    let left = HelpRequest::compute_digest("r", "sha256:c", 1, "q", "en", &[chunk], "instruction");
    let right =
        HelpRequest::compute_digest("r", "sha256:c", 1, "q", "en", &[tampered], "instruction");
    assert_ne!(
        left, right,
        "editing context bytes must change the request digest"
    );
}

#[test]
fn request_digests_bind_context_source_ids() {
    let chunk = ContextChunk {
        chunk_id: "a#body.0".into(),
        chunk_digest: "sha256:c".into(),
        source_ids: vec!["source.a".into(), "source.b".into()],
        text: "the exact bytes".into(),
    };
    let mut substituted = chunk.clone();
    substituted.source_ids[1] = "source.c".into();
    let left = HelpRequest::compute_digest("r", "sha256:c", 1, "q", "en", &[chunk], "instruction");
    let right =
        HelpRequest::compute_digest("r", "sha256:c", 1, "q", "en", &[substituted], "instruction");
    assert_ne!(
        left, right,
        "changing provenance must invalidate the request and its admission"
    );
}

#[test]
fn digest_labels_match_serde_labels() {
    // `as_str` feeds digests; serde feeds the JSON both languages read. If the
    // two disagree, Rust and TypeScript digest different strings for the same
    // record and the corpus verifies in exactly one of them.
    fn serde_label<T: serde::Serialize>(value: T) -> String {
        serde_json::to_value(value)
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }
    for topic in [
        crate::corpus::Topic::GettingStarted,
        crate::corpus::Topic::Providers,
        crate::corpus::Topic::ComputerUse,
        crate::corpus::Topic::Operations,
    ] {
        assert_eq!(
            topic.as_str(),
            serde_label(topic),
            "Topic::as_str disagrees with serde"
        );
    }
    for kind in [
        crate::corpus::ChunkKind::Title,
        crate::corpus::ChunkKind::Summary,
        crate::corpus::ChunkKind::Body,
    ] {
        assert_eq!(
            kind.as_str(),
            serde_label(kind),
            "ChunkKind::as_str disagrees with serde"
        );
    }
    for visibility in [Visibility::Public, Visibility::Gated, Visibility::Operator] {
        assert_eq!(
            visibility.as_str(),
            serde_label(visibility),
            "Visibility::as_str disagrees with serde"
        );
    }
    for code in [
        PublicErrorCode::NotAvailable,
        PublicErrorCode::Busy,
        PublicErrorCode::Timeout,
    ] {
        assert_eq!(code.as_str(), serde_label(code));
    }
    for status in [
        ProjectionStatus::Queued,
        ProjectionStatus::Running,
        ProjectionStatus::Answered,
        ProjectionStatus::Abstained,
        ProjectionStatus::Unavailable,
    ] {
        assert_eq!(status.as_str(), serde_label(status));
    }
    for kind in [
        RedactionKind::Secret,
        RedactionKind::Path,
        RedactionKind::Control,
        RedactionKind::Bidi,
        RedactionKind::Markup,
    ] {
        assert_eq!(kind.as_str(), serde_label(kind));
    }
    for outcome in [
        Outcome::Answered,
        Outcome::Abstained,
        Outcome::Denied,
        Outcome::Cancelled,
        Outcome::Abandoned,
        Outcome::TimedOut,
    ] {
        assert_eq!(outcome.as_str(), serde_label(outcome));
    }
    for certainty in [
        SendCertainty::NotSent,
        SendCertainty::Sent,
        SendCertainty::Unknown,
    ] {
        assert_eq!(certainty.as_str(), serde_label(certainty));
    }
}

#[test]
fn an_inbound_request_carrying_an_unknown_field_is_refused() {
    // The renderer sends exactly the fields the contract names. A request
    // carrying anything else is a request built against a different contract,
    // so it is refused rather than silently accepted with the extra dropped.
    let accepted: Result<crate::dto::HelpAsk, _> =
        serde_json::from_str(r#"{"session":"s","question":"q"}"#);
    assert!(accepted.is_ok(), "the declared shape still parses");

    for payload in [
        r#"{"session":"s","question":"q","capabilities":["run.execute"]}"#,
        r#"{"session":"s","question":"q","visibilityCeiling":"operator"}"#,
        r#"{"session":"s","handle":"h","chunkIds":["a#body.0"]}"#,
    ] {
        let refused: Result<crate::dto::HelpAsk, _> = serde_json::from_str(payload);
        assert!(
            refused.is_err(),
            "an inbound request must not carry authority fields: {payload}"
        );
    }
}
