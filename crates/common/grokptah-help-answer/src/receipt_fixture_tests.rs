//! Receipt shape parity with the browser broker client.
//!
//! The desktop reaches the executor through a Tauri command; the browser
//! reaches it through the broker. Both hand the same `ExecutionReceipt` to a
//! renderer, so if the Rust serialization and the TypeScript parser disagree
//! about a field name, one of those two paths silently shows nothing.
//!
//! This writes the serialization down as a fixture the TypeScript suite reads.
//! A rename on either side then has to be made on both, deliberately.

use std::path::PathBuf;

use crate::receipt::*;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("receipt-shape.json")
}

fn sample(outcome: ExecutionOutcome, failure: Option<FailureReason>) -> ExecutionReceipt {
    build_receipt(ReceiptInputs {
        admission_id: "sha256:admission".into(),
        request_digest: "sha256:request".into(),
        corpus_digest: "sha256:corpus".into(),
        index_digest: "sha256:index".into(),
        manifest_digest: "sha256:manifest".into(),
        grant_revision: 7,
        outcome,
        failure,
        outcome_digest: matches!(outcome, ExecutionOutcome::Answered)
            .then(|| "sha256:outcome".to_string()),
        cited_source_ids: vec!["durable.lifecycle".into()],
        claim_count: 2,
        queued_ms: 1,
        ran_ms: 12,
    })
}

#[test]
fn the_recorded_receipt_shape_still_matches_what_this_build_emits() {
    let receipts = vec![
        sample(ExecutionOutcome::Answered, None),
        sample(
            ExecutionOutcome::Denied,
            Some(FailureReason::AdmissionRefused),
        ),
        sample(
            ExecutionOutcome::Abandoned,
            Some(FailureReason::CallerCancelled),
        ),
    ];
    let emitted = serde_json::to_value(&receipts).expect("serializes");

    let recorded: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture_path()).expect("fixture is readable"),
    )
    .expect("fixture parses");
    assert_eq!(
        emitted, recorded,
        "receipt serialization changed; update fixtures/receipt-shape.json and the TypeScript peer together"
    );
}

#[test]
fn a_receipt_carries_no_artifact_field() {
    let value = serde_json::to_value(sample(ExecutionOutcome::Answered, None)).expect("serializes");
    let object = value.as_object().expect("object");
    for forbidden in [
        "answer",
        "query",
        "quote",
        "citations",
        "uncertainty",
        "text",
        "path",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "a receipt must not carry {forbidden}"
        );
    }
}

#[test]
fn an_absent_failure_is_omitted_rather_than_null() {
    // `null` and "not present" read differently to a strict parser, and the
    // TypeScript peer treats a missing key as "no failure".
    let value = serde_json::to_value(sample(ExecutionOutcome::Answered, None)).expect("serializes");
    let object = value.as_object().expect("object");
    assert!(!object.contains_key("failure"));
    assert!(object.contains_key("outcomeDigest"));

    let denied = serde_json::to_value(sample(
        ExecutionOutcome::Denied,
        Some(FailureReason::AdmissionRefused),
    ))
    .expect("serializes");
    let denied = denied.as_object().expect("object");
    assert_eq!(
        denied.get("failure").and_then(|v| v.as_str()),
        Some("admission_refused")
    );
    assert!(!denied.contains_key("outcomeDigest"));
}
