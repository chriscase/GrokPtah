//! Compatibility properties an external consumer depends on.
//!
//! The contract promises that a minor version bump is additive: a host may
//! learn a new word and an older consumer keeps working. These tests are what
//! makes that promise checkable rather than aspirational. Each one models the
//! same situation — a host one version ahead of this build — and asserts the
//! consumer degrades in a defined direction instead of failing.

use grokptah_agent_sdk::dto::*;
use grokptah_agent_sdk::prelude::*;
use serde_json::json;

/// Every wire vocabulary, with a token no build of this crate will ever know.
///
/// Kept as JSON round-trips rather than typed constructions so the assertion
/// is about the *wire*, which is what a consumer of a newer host actually
/// meets.
macro_rules! assert_open {
    ($ty:ty, $token:literal) => {{
        let decoded: $ty = serde_json::from_str(&format!("\"{}\"", $token))
            .unwrap_or_else(|e| panic!("{} rejected `{}`: {e}", stringify!($ty), $token));
        assert!(
            !decoded.is_known(),
            "{} claimed to know `{}`",
            stringify!($ty),
            $token
        );
        let reencoded = serde_json::to_string(&decoded).expect("re-serialize");
        assert_eq!(
            reencoded,
            format!("\"{}\"", $token),
            "{} rewrote the host's token instead of forwarding it",
            stringify!($ty)
        );
    }};
}

#[test]
fn every_wire_vocabulary_survives_a_word_this_build_does_not_have() {
    assert_open!(SessionKind, "ephemeral");
    assert_open!(RunLifecycle, "paused_for_review");
    assert_open!(StopCause, "provider_exhausted");
    assert_open!(ExecutionMode, "packaged_vm");
    assert_open!(VerificationStatus, "partially_verified");
    assert_open!(FollowUpDisposition, "coalesced");
    assert_open!(OperationClass, "provider_operation");
    assert_open!(ReceiptStatus, "superseded");
    assert_open!(DigestAlgorithm, "blake3");
    assert_open!(ArtifactMedia, "text/csv");
    assert_open!(ArtifactKind, "provider_receipt");
    assert_open!(ToolKind, "browse");
    assert_open!(ToolStatus, "deferred");
    assert_open!(TestOutcome, "flaked");
    assert_open!(PermissionOutcome, "escalated");
}

#[test]
fn a_known_token_still_decodes_to_its_variant() {
    // The open arm must not swallow the vocabulary it exists to protect.
    let known: RunLifecycle = serde_json::from_str("\"running\"").expect("known token");
    assert_eq!(known, RunLifecycle::Running);
    assert!(known.is_known());

    for token in RunLifecycle::known_tokens() {
        let decoded: RunLifecycle =
            serde_json::from_str(&format!("\"{token}\"")).expect("declared token decodes");
        assert!(
            decoded.is_known(),
            "`{token}` is advertised as known but decodes to Unknown"
        );
        assert_eq!(decoded.as_wire(), *token, "token is not its own round trip");
    }
}

#[test]
fn an_unrecognized_token_is_bounded_and_stripped() {
    // A host is not trusted to send a sane token. An unrecognized word reaches
    // a consumer's log or UI, so it goes through the same sanitizer every
    // other host-authored label does.
    let hostile: ToolKind = serde_json::from_str("\"we\\u001b[31mird\"").expect("decodes");
    assert!(
        !hostile.as_wire().contains('\u{1b}'),
        "an escape sequence reached the consumer: {:?}",
        hostile.as_wire()
    );
}

#[test]
fn a_lifecycle_this_build_cannot_read_is_not_treated_as_finished() {
    let unknown = RunLifecycle::from_wire("paused_for_review");
    // Fail closed: guessing "terminal" would stop a consumer observing a run
    // that may still be executing, and could report an outcome the host never
    // produced. Guessing "live" costs one more poll.
    assert!(!unknown.is_terminal());
    assert!(!unknown.is_known());
}

#[test]
fn a_receipt_status_this_build_cannot_read_is_uncertain() {
    let receipt = ReceiptView {
        request_id: RequestId::new("req-1").unwrap(),
        operation: OperationClass::from_wire("provider_operation"),
        status: ReceiptStatus::from_wire("superseded"),
        outcome: None,
        payload_digest: AttemptDigest::derive(b"salt", &"ab".repeat(32)),
        run_id: None,
        recorded_at: chrono::Utc::now(),
    };
    // The dangerous reading is "settled": it licenses a retry of a mutation
    // that may already have applied.
    assert!(receipt.is_uncertain());
    assert!(!receipt.is_settled());
}

#[test]
fn one_unrecognized_event_does_not_discard_the_page_around_it() {
    let page = json!([
        {"cursor": "c1", "at": "2026-01-01T00:00:00Z", "kind": "turn_started"},
        {"cursor": "c2", "at": "2026-01-01T00:00:01Z", "kind": "provider_operation",
         "operationId": "op-1", "detail": "whatever a newer host sends"},
        {"cursor": "c3", "at": "2026-01-01T00:00:02Z", "kind": "run_terminal",
         "lifecycle": "completed", "stopCause": "completed"}
    ]);

    let events: Vec<PublicEvent> = serde_json::from_value(page).expect("page decodes");
    assert_eq!(events.len(), 3, "the unknown event cost the page nothing");
    assert!(matches!(events[0].kind, PublicEventKind::TurnStarted));
    match &events[1].kind {
        PublicEventKind::Unrecognized { wire_kind } => {
            assert_eq!(wire_kind.as_str(), "provider_operation");
        }
        other => panic!("expected Unrecognized, got {other:?}"),
    }
    assert!(matches!(
        events[2].kind,
        PublicEventKind::RunTerminal { .. }
    ));
}

#[test]
fn a_known_event_kind_with_a_broken_field_still_fails() {
    // Tolerance is for vocabulary, not corruption. Collapsing a malformed
    // known kind into Unrecognized would hide a real bug behind the
    // forward-compatibility path.
    let broken = json!({
        "cursor": "c1", "at": "2026-01-01T00:00:00Z",
        "kind": "progress", "round": "not a number", "maxRounds": 8
    });
    assert!(serde_json::from_value::<PublicEvent>(broken).is_err());
}

#[test]
fn a_nested_unknown_token_does_not_fail_the_event_carrying_it() {
    // The kind is known; the tool vocabulary inside it is not. Before the
    // vocabularies were opened this failed the whole page.
    let event = json!({
        "cursor": "c9", "at": "2026-01-01T00:00:00Z", "kind": "tool_call",
        "callId": "call-1", "tool": "browse", "status": "deferred"
    });
    let decoded: PublicEvent = serde_json::from_value(event).expect("decodes");
    match decoded.kind {
        PublicEventKind::ToolCall { tool, status, .. } => {
            assert_eq!(tool.as_wire(), "browse");
            assert_eq!(status.as_wire(), "deferred");
            assert!(!tool.is_known() && !status.is_known());
        }
        other => panic!("expected ToolCall, got {other:?}"),
    }
}

#[tokio::test]
async fn a_run_view_from_a_newer_host_decodes_whole() {
    // The compound case, built from a *real* record rather than a hand-written
    // fixture: take a run this build produced, teach it three words this build
    // does not have, and read it back. A hand-rolled JSON body would drift
    // from the struct and stop testing anything.
    let plane = FakeControlPlane::builder().build();
    let session = plane.seeded_session().expect("builder seeds one session");
    let accepted = plane
        .submit_task(TaskSubmission {
            request_id: RequestId::new("req-0001").unwrap(),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            prompt: "anything".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");
    let view = plane
        .observe_run(RunSelector {
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect("observe");

    let mut wire = serde_json::to_value(&view).expect("serialize");
    wire["lifecycle"] = json!("paused_for_review");
    wire["executionMode"] = json!("packaged_vm");
    wire["stopCause"] = json!("provider_exhausted");

    let decoded: RunView = serde_json::from_value(wire).expect("run decodes");
    // Identity, revision, and timing are what a consumer's own bookkeeping
    // runs on. None of them are lost to a vocabulary it cannot read.
    assert_eq!(decoded.run_id, view.run_id);
    assert_eq!(decoded.revision, view.revision);
    assert_eq!(decoded.updated_at, view.updated_at);
    assert!(!decoded.lifecycle.is_known());
    assert!(!decoded.lifecycle.is_terminal());
    assert_eq!(decoded.execution_mode.as_wire(), "packaged_vm");
    assert_eq!(
        decoded.stop_cause.as_ref().map(|c| c.as_wire()),
        Some("provider_exhausted")
    );
}

#[test]
fn a_retention_window_declares_the_population_its_budget_counts() {
    // `max_receipts` alone is not interpretable. Under a host-wide budget a
    // consumer's own receipts can be expired by a neighbouring run's traffic,
    // and a consumer that read the number as a per-run allowance would report
    // a gap that is not a gap.
    let policy = ReceiptRetention::RUNTIME_DEFAULT;
    assert_eq!(policy.budget_scope, RetentionBudgetScope::Host);
    assert!(policy.exemptions.unsettled_retained);
    assert!(policy.exemptions.active_run_retained);

    let wire = serde_json::to_value(&policy).expect("serialize");
    assert_eq!(wire["budgetScope"], "host");
    assert_eq!(wire["exemptions"]["activeRunRetained"], true);
}

#[test]
fn an_unknown_media_type_is_opaque_bytes_rather_than_a_guess() {
    let media = ArtifactMedia::from_wire("text/html");
    // Rendering something the host never said was safe to render is how a
    // projection boundary turns into an injection surface.
    assert_eq!(media.media_type(), "application/octet-stream");
}
