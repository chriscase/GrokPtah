//! Deterministic headless-port tests.
//!
//! Every test drives the real port against the deterministic fake host, with
//! explicit instants and no clock, filesystem, provider, or network access.

use std::collections::BTreeSet;

use uuid::Uuid;

use super::fake::{
    instant, large_limits, small_limits, verified_evidence, FakeAuthority, FakeHost,
};
use super::port::HeadlessAgentPort;
use super::projection::{PortEventKind, PortRunOutcome};
use super::types::{
    PortDelivery, PortError, PortErrorCode, PortEvidenceGap, PortEvidenceSummary,
    PortExecutionMode, PortLimits, PortOperation, PortPrincipal, PortPromotionState, PortResult,
    PortReviewFacts, PortRunState, PortSubmitRequest, PortTier, PortVerification,
    MAX_PORT_EVENT_PAGE,
};
use super::PortBinding;

const WORKSPACE: &str = "/workspaces/contextdesk";

struct Harness {
    host: std::sync::Arc<FakeHost>,
    port: HeadlessAgentPort<FakeAuthority>,
    session_id: Uuid,
    principal: PortPrincipal,
}

impl Harness {
    fn new() -> Self {
        Self::with_tier(PortTier::Coordinator)
    }

    fn with_tier(tier: PortTier) -> Self {
        let session_id = Uuid::from_u128(0x5eed_5eed_5eed_5eed_5eed_5eed_5eed_5eed);
        let host = FakeHost::new(session_id, WORKSPACE);
        let port = HeadlessAgentPort::new(host.authority());
        let principal = PortPrincipal::new("owner-1", "contextdesk-laptop", tier).unwrap();
        Self {
            host,
            port,
            session_id,
            principal,
        }
    }

    async fn bind(&self) -> PortBinding {
        let negotiation = self.port.negotiate(&self.principal).await.unwrap();
        PortBinding::bind(
            &negotiation,
            self.principal.clone(),
            self.session_id,
            WORKSPACE,
        )
        .unwrap()
    }

    fn request(&self, request_id: &str, prompt: &str) -> PortSubmitRequest {
        PortSubmitRequest::new(request_id, prompt).unwrap()
    }
}

fn code<T>(result: PortResult<T>) -> PortErrorCode {
    result.err().expect("expected failure").code
}

#[tokio::test]
async fn submit_delivers_and_projects_the_run() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let view = harness
        .port
        .submit(&binding, &harness.request("req-1", "build it"), instant(11))
        .await
        .unwrap();

    assert_eq!(view.receipt.delivery, PortDelivery::Delivered);
    assert!(!view.receipt.retry_with_same_request_id);
    let run = view.run.expect("delivered submit projects its run");
    assert_eq!(run.outcome, PortRunOutcome::Running);
    assert_eq!(run.request_id, "req-1");
    assert_eq!(harness.host.performed_submits(), 1);
    // The admitted limits are the negotiated ones, echoed back typed.
    assert_eq!(view.receipt.admitted_limits, Some(large_limits()));
}

// ── restart ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn restart_makes_an_unsettled_claim_uncertain_and_never_replays_it() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    harness.host.interrupt_next_submit();

    // The effect landed; the acknowledgement did not.
    let interrupted = harness
        .port
        .submit(&binding, &harness.request("req-a", "do work"), instant(11))
        .await;
    assert_eq!(code(interrupted), PortErrorCode::Internal);
    assert_eq!(harness.host.performed_submits(), 1);

    // Before restart the claim belongs to this generation: still sending.
    let sending = harness
        .port
        .submit(&binding, &harness.request("req-a", "do work"), instant(12))
        .await
        .unwrap();
    assert_eq!(sending.receipt.delivery, PortDelivery::Sending);
    assert!(!sending.receipt.retry_with_same_request_id);
    assert_eq!(
        harness.host.performed_submits(),
        1,
        "no replay while sending"
    );

    // Restart: a new generation, the claim settles interrupted, the live run
    // becomes interrupted, and nothing is replayed.
    harness.host.restart(instant(100));
    let binding = harness.bind().await;
    let uncertain = harness
        .port
        .submit(&binding, &harness.request("req-a", "do work"), instant(101))
        .await
        .unwrap();
    assert_eq!(uncertain.receipt.delivery, PortDelivery::Uncertain);
    assert!(
        !uncertain.receipt.retry_with_same_request_id,
        "uncertain must never be retryable under the same request id"
    );
    assert_eq!(harness.host.performed_submits(), 1, "no auto-replay");
    // The run the interrupted effect produced stays visible.
    let run = uncertain
        .run
        .expect("uncertain delivery keeps its run visible");
    assert_eq!(run.state, PortRunState::Interrupted);
    assert_eq!(run.outcome, PortRunOutcome::Interrupted);

    // A fresh request id is the only way forward, and it does send.
    let fresh = harness
        .port
        .submit(&binding, &harness.request("req-b", "do work"), instant(102))
        .await
        .unwrap();
    assert_eq!(fresh.receipt.delivery, PortDelivery::Delivered);
    assert_eq!(harness.host.performed_submits(), 2);
}

#[tokio::test]
async fn a_run_with_no_acknowledged_claim_is_uncertain() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    harness.host.insert_orphan_run("req-orphan");

    let view = harness
        .port
        .submit(
            &binding,
            &harness.request("req-orphan", "resend me"),
            instant(11),
        )
        .await
        .unwrap();
    assert_eq!(view.receipt.delivery, PortDelivery::Uncertain);
    assert_eq!(harness.host.performed_submits(), 0, "never replayed");
}

// ── stale revision ─────────────────────────────────────────────────────────

#[tokio::test]
async fn a_moved_capability_revision_invalidates_every_operation() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let view = harness
        .port
        .submit(&binding, &harness.request("req-1", "hello"), instant(11))
        .await
        .unwrap();
    let run_id = view.receipt.run_id.clone().unwrap();

    // The host changes its declared limits, which changes the revision.
    harness.host.set_limits(small_limits());

    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-2", "hi"), instant(12))
                .await
        ),
        PortErrorCode::StaleBinding
    );
    assert_eq!(
        code(
            harness
                .port
                .events(&binding, &run_id, 0, 10, instant(12))
                .await
        ),
        PortErrorCode::StaleBinding
    );
    assert_eq!(
        code(harness.port.review(&binding, &run_id).await),
        PortErrorCode::StaleBinding
    );
    assert_eq!(
        code(
            harness
                .port
                .cancel(&binding, "req-3", &run_id, instant(12))
                .await
        ),
        PortErrorCode::StaleBinding
    );
    assert_eq!(
        harness.host.performed_submits(),
        1,
        "a stale binding must not reach an effect"
    );

    // Rebinding against the current negotiation restores service.
    let rebound = harness.bind().await;
    assert!(harness
        .port
        .events(&rebound, &run_id, 0, 10, instant(13))
        .await
        .is_ok());
}

#[tokio::test]
async fn an_undeclared_capability_is_unsupported_not_forbidden() {
    let harness = Harness::new();
    harness
        .host
        .set_capabilities([PortOperation::Events, PortOperation::Review]);
    let binding = harness.bind().await;
    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::Unsupported
    );
}

#[tokio::test]
async fn a_host_that_cannot_negotiate_stops_the_operation() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    harness.host.fail_negotiation(PortError::new(
        PortErrorCode::Unavailable,
        "host is draining",
    ));
    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::Unavailable
    );
    assert_eq!(harness.host.performed_submits(), 0);
}

// ── wrong scope ────────────────────────────────────────────────────────────

#[tokio::test]
async fn every_out_of_scope_read_is_the_identical_forbidden_scope_failure() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let view = harness
        .port
        .submit(&binding, &harness.request("req-1", "hello"), instant(11))
        .await
        .unwrap();
    let run_id = view.receipt.run_id.unwrap();

    let negotiation = harness.port.negotiate(&harness.principal).await.unwrap();
    let other_session = PortBinding::bind(
        &negotiation,
        harness.principal.clone(),
        Uuid::from_u128(0x1234),
        WORKSPACE,
    )
    .unwrap();
    let other_workspace = PortBinding::bind(
        &negotiation,
        harness.principal.clone(),
        harness.session_id,
        "/workspaces/other",
    )
    .unwrap();

    let baseline = harness
        .port
        .events(&binding, "run-does-not-exist", 0, 10, instant(12))
        .await
        .unwrap_err();
    assert_eq!(baseline.code, PortErrorCode::ForbiddenScope);

    for failure in [
        harness
            .port
            .events(&other_session, &run_id, 0, 10, instant(12))
            .await
            .unwrap_err(),
        harness
            .port
            .events(&other_workspace, &run_id, 0, 10, instant(12))
            .await
            .unwrap_err(),
        harness
            .port
            .events(&binding, "../escape", 0, 10, instant(12))
            .await
            .unwrap_err(),
        harness
            .port
            .events(&binding, "", 0, 10, instant(12))
            .await
            .unwrap_err(),
        harness
            .port
            .review(&other_session, &run_id)
            .await
            .unwrap_err(),
        harness
            .port
            .review(&binding, "run-does-not-exist")
            .await
            .unwrap_err(),
    ] {
        assert_eq!(
            failure, baseline,
            "scoped failures must be byte-identical so they cannot be an existence oracle"
        );
    }
}

#[tokio::test]
async fn an_observer_cannot_mutate_and_a_worker_cannot_submit() {
    let observer = Harness::with_tier(PortTier::Observer);
    let binding = observer.bind().await;
    assert_eq!(
        code(
            observer
                .port
                .submit(&binding, &observer.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
    assert_eq!(
        code(
            observer
                .port
                .cancel(&binding, "req-2", "run-1", instant(11))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
    assert_eq!(observer.host.performed_submits(), 0);

    let worker = Harness::with_tier(PortTier::Worker);
    let worker_binding = worker.bind().await;
    assert_eq!(
        code(
            worker
                .port
                .submit(&worker_binding, &worker.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
}

#[test]
fn delegation_may_only_narrow() {
    let coordinator = PortPrincipal::new("owner-1", "cred-1", PortTier::Coordinator).unwrap();
    let worker = coordinator
        .delegate("owner-1", "cred-worker", PortTier::Worker)
        .unwrap();
    assert_eq!(worker.tier, PortTier::Worker);
    assert_eq!(worker.delegated_from.as_deref(), Some("owner-1"));
    assert!(coordinator
        .delegate("owner-1", "cred-op", PortTier::LocalOperator)
        .is_err());
    assert!(worker
        .delegate("owner-1", "cred-coord", PortTier::Coordinator)
        .is_err());
}

#[tokio::test]
async fn authorization_revoked_after_negotiation_stops_the_effect() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    // Negotiation succeeded; authority is withdrawn before the effect.
    harness.host.revoke_authorization();
    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
    assert_eq!(
        harness.host.performed_submits(),
        0,
        "the effect-boundary recheck must run before the effect, not after"
    );
    assert!(harness.host.run_ids().is_empty());
}

// ── page gaps and cursors ──────────────────────────────────────────────────

#[tokio::test]
async fn event_pages_are_bounded_monotonic_and_resumable() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness.host.append_events(
        &run_id,
        &[
            PortEventKind::TurnStarted,
            PortEventKind::ToolCallStarted,
            PortEventKind::ToolCallCompleted,
            PortEventKind::FileEdited,
            PortEventKind::TurnComplete,
        ],
    );

    let first = harness
        .port
        .events(&binding, &run_id, 0, 2, instant(12))
        .await
        .unwrap();
    assert_eq!(first.page.entries.len(), 2);
    assert_eq!(first.page.applied_limit, 2);
    assert!(!first.page.cursor_expired);
    let cursor = first.page.next_cursor.expect("more entries remain");
    assert_eq!(cursor, first.page.entries.last().unwrap().seq);
    assert!(first.page.entries.windows(2).all(|w| w[0].seq < w[1].seq));

    let second = harness
        .port
        .events(&binding, &run_id, cursor, 100, instant(12))
        .await
        .unwrap();
    assert!(second.page.entries.iter().all(|entry| entry.seq > cursor));
    assert_eq!(second.page.entries.len(), 3);
    assert_eq!(
        second.page.next_cursor, None,
        "next_cursor is present only while more entries remain"
    );
}

#[tokio::test]
async fn a_page_limit_is_clamped_to_the_negotiated_limit() {
    let harness = Harness::new();
    harness.host.set_limits(small_limits());
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness
        .host
        .append_events(&run_id, &[PortEventKind::Progress; 6]);

    let page = harness
        .port
        .events(&binding, &run_id, 0, usize::MAX, instant(12))
        .await
        .unwrap()
        .page;
    assert_eq!(page.applied_limit, small_limits().max_event_page);
    assert_eq!(page.entries.len(), small_limits().max_event_page);
    assert!(page.applied_limit <= MAX_PORT_EVENT_PAGE);
}

#[tokio::test]
async fn an_expired_cursor_is_reported_as_a_gap_not_a_short_stream() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness
        .host
        .append_events(&run_id, &[PortEventKind::Progress; 8]);
    // The bounded ring evicts everything below seq 5.
    harness.host.expire_journal_below(5);

    let page = harness
        .port
        .events(&binding, &run_id, 0, 100, instant(12))
        .await
        .unwrap()
        .page;
    assert!(page.cursor_expired);
    assert!(page.entries.is_empty());
    assert_eq!(page.next_cursor, None);

    // A cursor inside the retained window still resumes exactly.
    let resumed = harness
        .port
        .events(&binding, &run_id, 5, 100, instant(12))
        .await
        .unwrap()
        .page;
    assert!(!resumed.cursor_expired);
    assert!(resumed.entries.iter().all(|entry| entry.seq > 5));
}

// ── cancel ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancel_is_a_durable_effect_and_is_never_performed_twice() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();

    let cancelled = harness
        .port
        .cancel(&binding, "req-cancel", &run_id, instant(12))
        .await
        .unwrap();
    assert_eq!(cancelled.receipt.delivery, PortDelivery::Delivered);
    assert_eq!(
        cancelled.run.as_ref().map(|run| run.outcome),
        Some(PortRunOutcome::Cancelled)
    );
    assert_eq!(harness.host.performed_cancels(), 1);

    // The same request id replays the durable receipt instead of re-cancelling.
    let replay = harness
        .port
        .cancel(&binding, "req-cancel", &run_id, instant(13))
        .await
        .unwrap();
    assert_eq!(replay.receipt.delivery, PortDelivery::Delivered);
    assert_eq!(harness.host.performed_cancels(), 1);
}

#[tokio::test]
async fn cancel_of_an_out_of_scope_run_never_reaches_the_effect() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    assert_eq!(
        code(
            harness
                .port
                .cancel(&binding, "req-cancel", "run-nope", instant(12))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
    assert_eq!(harness.host.performed_cancels(), 0);
}

// ── typed evidence for terminal completion ────────────────────────────────

#[tokio::test]
async fn a_terminal_completion_without_typed_evidence_is_not_verified() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness
        .host
        .append_events(&run_id, &[PortEventKind::TurnComplete]);
    harness.host.finish_run(
        &run_id,
        PortRunState::Completed,
        PortEvidenceSummary {
            changed_files: 3,
            usage_complete: false,
            usage_pending_requests: 2,
            verification: None,
            ..PortEvidenceSummary::default()
        },
    );

    let run = harness
        .port
        .events(&binding, &run_id, 0, 10, instant(13))
        .await
        .unwrap()
        .run;
    assert_eq!(run.outcome, PortRunOutcome::CompletedUnverified);
    let gaps: BTreeSet<PortEvidenceGap> = run.evidence_gaps.iter().copied().collect();
    assert!(gaps.contains(&PortEvidenceGap::MissingVerification));
    assert!(gaps.contains(&PortEvidenceGap::IncompleteUsage));
    assert!(gaps.contains(&PortEvidenceGap::PendingProviderAttempts));
}

#[tokio::test]
async fn a_terminal_completion_with_typed_evidence_is_verified() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness
        .host
        .append_events(&run_id, &[PortEventKind::TurnComplete]);
    harness
        .host
        .finish_run(&run_id, PortRunState::Completed, verified_evidence());

    let run = harness
        .port
        .events(&binding, &run_id, 0, 10, instant(13))
        .await
        .unwrap()
        .run;
    assert_eq!(run.outcome, PortRunOutcome::CompletedVerified);
    assert!(run.evidence_gaps.is_empty());
    assert_eq!(run.evidence.verification, Some(PortVerification::Verified));

    // An unverified classification is still not a verified completion.
    harness.host.finish_run(
        &run_id,
        PortRunState::Completed,
        PortEvidenceSummary {
            verification: Some(PortVerification::Unverified),
            usage_complete: true,
            ..verified_evidence()
        },
    );
    let run = harness
        .port
        .events(&binding, &run_id, 0, 10, instant(14))
        .await
        .unwrap()
        .run;
    assert_eq!(run.outcome, PortRunOutcome::CompletedUnverified);
    assert_eq!(
        run.evidence_gaps,
        vec![PortEvidenceGap::UnverifiedVerification]
    );
}

// ── redaction ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn no_projection_can_carry_a_prompt_path_credential_or_model_output() {
    const PROMPT: &str = "SECRET_PROMPT refactor /private/home/user/.ssh/id_rsa with sk-live-TOKEN";
    let harness = Harness::new();
    let binding = harness.bind().await;
    let view = harness
        .port
        .submit(&binding, &harness.request("req-1", PROMPT), instant(11))
        .await
        .unwrap();
    let run_id = view.receipt.run_id.clone().unwrap();
    harness.host.append_events(
        &run_id,
        &[
            PortEventKind::ModelOutput,
            PortEventKind::ShellStarted,
            PortEventKind::FileEdited,
        ],
    );
    harness
        .host
        .finish_run(&run_id, PortRunState::Completed, verified_evidence());
    harness.host.set_review(PortReviewFacts {
        run_id: run_id.clone(),
        promotion: PortPromotionState::Ready,
        source_fingerprint: Some("abc123".into()),
        final_fingerprint: Some("def456".into()),
        changed_file_count: 2,
        diff_available: true,
        diff_truncated: true,
    });

    let events = harness
        .port
        .events(&binding, &run_id, 0, 100, instant(13))
        .await
        .unwrap();
    let review = harness.port.review(&binding, &run_id).await.unwrap();

    let serialized = [
        serde_json::to_string(&view).unwrap(),
        serde_json::to_string(&events).unwrap(),
        serde_json::to_string(&review).unwrap(),
    ]
    .join("\n");
    for forbidden in [
        "SECRET_PROMPT",
        "id_rsa",
        "sk-live-TOKEN",
        "/private/home",
        WORKSPACE,
        "refactor",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "public projections must not carry {forbidden}"
        );
    }

    // The run projection's key set is the redaction contract. A new field that
    // could hold prompt, path, or model text has to change this assertion.
    let run_keys: BTreeSet<String> = serde_json::to_value(&events.run)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        run_keys,
        BTreeSet::from(
            [
                "runId",
                "sessionId",
                "requestId",
                "state",
                "outcome",
                "delivery",
                "round",
                "maxRounds",
                "admittedLimits",
                "evidence",
                "evidenceGaps",
                "stopCause",
                "promotion",
                "range",
                "createdAt",
                "updatedAt",
                "ageMillis",
            ]
            .map(String::from)
        )
    );

    // Event entries carry a sequence and a classified kind, nothing else.
    let entry_keys: BTreeSet<String> = serde_json::to_value(events.page.entries.first().unwrap())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        entry_keys,
        BTreeSet::from(["seq", "kind"].map(String::from))
    );
}

#[test]
fn a_port_error_message_is_fixed_host_authored_text() {
    // The only constructor takes `&'static str`, so a provider or model string
    // cannot become an error message. This test documents the guarantee.
    let error = PortError::new(PortErrorCode::LimitExceeded, "requested bounds exceed");
    assert_eq!(error.code.as_str(), "limit_exceeded");
    assert_eq!(error.message, "requested bounds exceed");
}

// ── small and large model bounds ───────────────────────────────────────────

#[tokio::test]
async fn bounds_come_from_the_fresh_negotiation_not_from_bind_time() {
    let harness = Harness::new();
    harness.host.set_limits(small_limits());
    let binding = harness.bind().await;
    let long_prompt = "x".repeat(small_limits().max_prompt_bytes + 1);

    assert_eq!(
        code(
            harness
                .port
                .submit(
                    &binding,
                    &harness.request("req-1", &long_prompt),
                    instant(11)
                )
                .await
        ),
        PortErrorCode::LimitExceeded
    );
    assert_eq!(harness.host.performed_submits(), 0);

    // The host grows its limits; the old binding is now stale, and the new one
    // admits the same prompt.
    harness.host.set_limits(large_limits());
    let rebound = harness.bind().await;
    let view = harness
        .port
        .submit(
            &rebound,
            &harness.request("req-1", &long_prompt),
            instant(12),
        )
        .await
        .unwrap();
    assert_eq!(view.receipt.delivery, PortDelivery::Delivered);
    assert_eq!(
        view.receipt.admitted_limits.map(|l| l.max_prompt_bytes),
        Some(large_limits().max_prompt_bytes)
    );
}

#[tokio::test]
async fn a_caller_may_narrow_bounds_but_never_widen_them() {
    let harness = Harness::new();
    harness.host.set_limits(small_limits());
    let binding = harness.bind().await;

    let narrowed = super::types::PortRunBounds {
        max_rounds: Some(1),
        max_total_tokens: Some(1_000),
        ..Default::default()
    };
    let view = harness
        .port
        .submit(
            &binding,
            &harness.request("req-1", "hi").with_bounds(narrowed),
            instant(11),
        )
        .await
        .unwrap();
    let admitted = view.receipt.admitted_limits.unwrap();
    assert_eq!(admitted.max_rounds, 1);
    assert_eq!(admitted.max_total_tokens, Some(1_000));
    assert_eq!(admitted.max_prompt_bytes, small_limits().max_prompt_bytes);

    let widened = super::types::PortRunBounds {
        max_rounds: Some(large_limits().max_rounds),
        ..Default::default()
    };
    assert_eq!(
        code(
            harness
                .port
                .submit(
                    &binding,
                    &harness.request("req-2", "hi").with_bounds(widened),
                    instant(12),
                )
                .await
        ),
        PortErrorCode::LimitExceeded
    );
    assert_eq!(harness.host.performed_submits(), 1);
}

#[test]
fn a_zero_or_absurd_negotiated_limit_fails_closed() {
    let bad = PortLimits {
        max_prompt_bytes: 0,
        ..large_limits()
    };
    assert_eq!(bad.validate().unwrap_err().code, PortErrorCode::Unavailable);
    let good = large_limits();
    assert!(good.validate().is_ok());
    assert_eq!(good.clamp_page(0), super::types::DEFAULT_PORT_EVENT_PAGE);
    assert_eq!(good.clamp_page(usize::MAX), good.max_event_page);
}

// ── review ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn review_exposes_a_decision_surface_without_the_work() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    harness
        .host
        .append_events(&run_id, &[PortEventKind::FileEdited]);
    harness
        .host
        .finish_run(&run_id, PortRunState::Completed, verified_evidence());
    harness.host.set_review(PortReviewFacts {
        run_id: run_id.clone(),
        promotion: PortPromotionState::Ready,
        source_fingerprint: Some("source-fp".into()),
        final_fingerprint: Some("final-fp".into()),
        changed_file_count: 4,
        diff_available: true,
        diff_truncated: false,
    });

    let review = harness.port.review(&binding, &run_id).await.unwrap();
    assert_eq!(review.promotion, PortPromotionState::Ready);
    assert_eq!(review.changed_file_count, 4);
    assert!(review.diff_available);
    assert_eq!(review.outcome, PortRunOutcome::CompletedVerified);
    assert!(review.evidence_gaps.is_empty());

    let keys: BTreeSet<String> = serde_json::to_value(&review)
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        keys,
        BTreeSet::from(
            [
                "runId",
                "outcome",
                "promotion",
                "sourceFingerprint",
                "finalFingerprint",
                "changedFileCount",
                "diffAvailable",
                "diffTruncated",
                "evidence",
                "evidenceGaps",
            ]
            .map(String::from)
        ),
        "review must stay a decision surface: no diff text, no changed paths"
    );
}

// ── binding hygiene ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_binding_cannot_be_minted_for_a_traversal_or_empty_scope() {
    let harness = Harness::new();
    let negotiation = harness.port.negotiate(&harness.principal).await.unwrap();
    for workspace in ["", "   ", "/workspaces/../etc", "..", "a/../b"] {
        assert!(
            PortBinding::bind(
                &negotiation,
                harness.principal.clone(),
                harness.session_id,
                workspace,
            )
            .is_err(),
            "workspace {workspace:?} must not bind"
        );
    }
    assert!(PortBinding::bind(
        &negotiation,
        harness.principal.clone(),
        Uuid::nil(),
        WORKSPACE
    )
    .is_err());
}

#[test]
fn principal_identifiers_are_bounded_and_shape_checked() {
    assert!(PortPrincipal::new("", "cred", PortTier::Observer).is_err());
    assert!(PortPrincipal::new("owner", "..", PortTier::Observer).is_err());
    assert!(PortPrincipal::new("owner/../root", "cred", PortTier::Observer).is_err());
    assert!(PortPrincipal::new("a".repeat(257), "cred", PortTier::Observer).is_err());
    assert!(PortPrincipal::new("owner-1.a_b", "cred-1", PortTier::Observer).is_ok());
}

#[tokio::test]
async fn an_authorization_that_does_not_describe_the_effect_is_refused() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    harness.host.misissue_authorization();
    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-1", "hi"), instant(11))
                .await
        ),
        PortErrorCode::ForbiddenScope
    );
    assert_eq!(harness.host.performed_submits(), 0);
}

// ── reuse and host neutrality ─────────────────────────────────────────────

/// The port must not grow a second send engine, and its core must not learn
/// about any particular host. Both are structural properties, so they are
/// asserted against the sources rather than only through behaviour.
#[test]
fn the_port_delegates_to_the_shipped_runtime_and_stays_host_neutral() {
    let adapter = include_str!("orchestration_authority.rs");
    for delegated in [
        "submit_task_with_execution_mode_and_queue",
        "get_events_scoped",
        "review_run",
        "authorize_run_request",
        "recheck_build_scope",
        "load_idempotency",
    ] {
        assert!(
            adapter.contains(delegated),
            "the adapter must delegate to the shipped runtime method {delegated}"
        );
    }
    for forbidden in ["reqwest", "tokio::spawn", "ProviderProfile", "tokio::fs"] {
        assert!(
            !adapter.contains(forbidden),
            "the adapter must not contain a send engine ({forbidden})"
        );
    }

    for (name, source) in [
        ("port.rs", include_str!("port.rs")),
        ("types.rs", include_str!("types.rs")),
        ("projection.rs", include_str!("projection.rs")),
        ("authority.rs", include_str!("authority.rs")),
    ] {
        for forbidden in [
            "AgentHost",
            "OrchestrationService",
            "reqwest",
            "axum",
            "tauri",
            "mcp_control",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} must stay host-neutral; it referenced {forbidden}"
            );
        }
    }
}

#[tokio::test]
async fn submit_defaults_to_shared_execution_and_can_request_a_reviewable_run() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap();
    assert_eq!(
        harness.host.last_execution_mode(),
        Some(PortExecutionMode::Shared),
        "a submit that does not ask for isolation must not get it"
    );

    harness
        .port
        .submit(
            &binding,
            &harness
                .request("req-2", "hi")
                .with_execution_mode(PortExecutionMode::IsolatedWorktree),
            instant(12),
        )
        .await
        .unwrap();
    assert_eq!(
        harness.host.last_execution_mode(),
        Some(PortExecutionMode::IsolatedWorktree)
    );
}

#[tokio::test]
async fn a_request_id_belongs_to_one_operation_and_one_run() {
    let harness = Harness::new();
    let binding = harness.bind().await;
    let run_id = harness
        .port
        .submit(&binding, &harness.request("req-1", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();
    let second = harness
        .port
        .submit(&binding, &harness.request("req-2", "hi"), instant(11))
        .await
        .unwrap()
        .receipt
        .run_id
        .unwrap();

    // The submit's request id cannot be reused to cancel.
    assert_eq!(
        code(
            harness
                .port
                .cancel(&binding, "req-1", &run_id, instant(12))
                .await
        ),
        PortErrorCode::Conflict
    );
    // A cancel's request id cannot be re-pointed at another run.
    harness
        .port
        .cancel(&binding, "req-cancel", &run_id, instant(12))
        .await
        .unwrap();
    assert_eq!(
        code(
            harness
                .port
                .cancel(&binding, "req-cancel", &second, instant(13))
                .await
        ),
        PortErrorCode::Conflict
    );
    // And a cancel's request id cannot be reused to submit.
    assert_eq!(
        code(
            harness
                .port
                .submit(&binding, &harness.request("req-cancel", "hi"), instant(13))
                .await
        ),
        PortErrorCode::Conflict
    );
    assert_eq!(harness.host.performed_cancels(), 1);
    assert_eq!(harness.host.performed_submits(), 2);
}
