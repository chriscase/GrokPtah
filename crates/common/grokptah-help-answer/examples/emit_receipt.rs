//! Emit the receipt shape fixture the TypeScript broker peer reads.
//!
//! Run with `cargo run -p grokptah-help-answer --example emit_receipt`, which
//! writes `fixtures/receipt-shape.json`. Regenerating is deliberate: the
//! fixture is how a rename on one side is forced to be made on both.

use grokptah_help_answer::{
    ExecutionOutcome, ExecutionReceipt, FailureReason, ReceiptInputs, build_receipt,
};

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

fn main() -> std::io::Result<()> {
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
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("receipt-shape.json");
    let json = serde_json::to_string_pretty(&receipts).expect("serializes");
    std::fs::write(&path, format!("{json}\n"))?;
    println!("wrote {}", path.display());
    Ok(())
}
