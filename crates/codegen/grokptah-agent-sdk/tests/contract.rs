//! Contract tests for the GrokPtah agent capability boundary.
//!
//! These drive the deterministic fake through every scenario the seam claims
//! to handle. They are the executable half of `docs/AGENT_SDK_SEAM.md`: if a
//! statement in that document is not checked here, it is a claim, not a
//! contract.

use grokptah_agent_sdk::conformance::{self, CheckOutcome, Harness};
use grokptah_agent_sdk::prelude::*;

use std::sync::atomic::{AtomicU64, Ordering};

fn session_of(plane: &FakeControlPlane) -> SessionView {
    plane.seeded_session().expect("builder seeds one session")
}

fn request_id(n: u64) -> RequestId {
    RequestId::new(format!("req-{n:04}")).expect("minted request id is valid")
}

fn submission(session: &SessionView, n: u64, prompt: &str) -> TaskSubmission {
    TaskSubmission {
        request_id: request_id(n),
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        prompt: prompt.to_string(),
        bounds: None,
        execution_mode: ExecutionMode::Shared,
        allow_queue: false,
    }
}

fn selector(session: &SessionView, run_id: &RunId) -> RunSelector {
    RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: run_id.clone(),
    }
}

// ── Success path ──────────────────────────────────────────────────────────

#[tokio::test]
async fn happy_path_submit_observe_stream_and_complete() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);

    let connected = plane.connect().await.expect("connect");
    assert!(!connected.is_degraded());
    connected
        .require(&CapabilityId::TaskSubmit)
        .expect("task.submit is advertised");

    let accepted = plane
        .submit_task(submission(&session, 1, "do the thing"))
        .await
        .expect("submit");
    assert_eq!(accepted.lifecycle, RunLifecycle::Queued);
    assert!(!accepted.replayed);

    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");

    let view = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    assert_eq!(view.lifecycle, RunLifecycle::Completed);
    assert_eq!(view.stop_cause, Some(StopCause::Completed));
    assert!(view.revision.is_newer_than(accepted.revision));
    assert_eq!(
        view.verification.map(|v| v.status),
        Some(VerificationStatus::Verified)
    );

    let page = plane
        .stream_events(selector(&session, &accepted.run_id), PageRequest::new())
        .await
        .expect("events");
    assert!(!page.items.is_empty());
    assert!(page.is_caught_up());
    assert!(matches!(
        page.items.last().map(|e| &e.kind),
        Some(PublicEventKind::RunTerminal { .. })
    ));
}

#[tokio::test]
async fn public_projection_carries_no_prompt_or_path_or_secret() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let secret_prompt = "SECRET-PROMPT-TEXT-do-not-echo";

    let accepted = plane
        .submit_task(submission(&session, 1, secret_prompt))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");

    let view = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    let json = serde_json::to_string(&view).expect("serialize run view");

    // The prompt goes in and never comes back out.
    assert!(!json.contains(secret_prompt), "{json}");
    // No absolute host path can appear: the only path-shaped field is a
    // validated RelativePath, and the workspace is an opaque ref.
    assert!(!json.contains("/home/"), "{json}");
    assert!(!json.contains("GROKPTAH_HOME"), "{json}");
    for changed in &view.changed_files {
        assert!(!changed.path.as_str().starts_with('/'));
        assert!(!changed.path.as_str().contains(".."));
    }

    let events = plane
        .stream_events(selector(&session, &accepted.run_id), PageRequest::new())
        .await
        .expect("events");
    let events_json = serde_json::to_string(&events).expect("serialize events");
    assert!(!events_json.contains(secret_prompt), "{events_json}");
}

// ── Replay / idempotency ──────────────────────────────────────────────────

#[tokio::test]
async fn exact_replay_returns_the_original_run_without_doing_work_twice() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let request = submission(&session, 1, "one instruction");

    let first = plane.submit_task(request.clone()).await.expect("first");
    let second = plane.submit_task(request).await.expect("replay");

    assert_eq!(first.run_id, second.run_id);
    assert!(!first.replayed);
    assert!(second.replayed, "a replay must announce itself");
}

#[tokio::test]
async fn reused_key_with_a_different_payload_is_a_conflict() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);

    plane
        .submit_task(submission(&session, 1, "first instruction"))
        .await
        .expect("first");
    let error = plane
        .submit_task(submission(&session, 1, "second instruction"))
        .await
        .expect_err("reused key must fail closed");

    assert_eq!(error.code, SdkErrorCode::Conflict);
    assert_eq!(error.retry_disposition(), RetryDisposition::Never);
}

#[tokio::test]
async fn a_timeout_is_recoverable_by_replaying_the_same_key() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let request = submission(&session, 1, "instruction");

    plane.inject_for(Operation::SubmitTask, Fault::Timeout);
    let error = plane
        .submit_task(request.clone())
        .await
        .expect_err("armed timeout");
    assert_eq!(error.code, SdkErrorCode::Timeout);

    // The SDK, not the consumer, decides that this is safe to retry.
    match recover_mutation(&request.request_id, &error) {
        MutationRecovery::RetrySameKey(key) => assert_eq!(key, request.request_id),
        other => panic!("expected RetrySameKey, got {other:?}"),
    }
    let accepted = plane.submit_task(request).await.expect("retry succeeds");
    assert!(!accepted.replayed, "the first attempt never landed");
}

// ── Stale observation ─────────────────────────────────────────────────────

#[tokio::test]
async fn an_out_of_order_snapshot_is_rejected_rather_than_applied() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");

    let early = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    plane.start_run(&accepted.run_id).expect("start");
    let later = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    assert!(later.revision.is_newer_than(early.revision));

    let mut watermark = RevisionWatermark::new();
    watermark.admit(later.revision).expect("newest applies");

    // A late-delivered older snapshot must not regress the view.
    let error = watermark
        .admit(early.revision)
        .expect_err("older snapshot must be refused");
    assert_eq!(error.code, SdkErrorCode::StaleObservation);
    assert_eq!(watermark.applied(), later.revision);
}

#[tokio::test]
async fn a_stale_revision_fence_rejects_the_mutation_without_effect() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);

    let accepted = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "first follow-up".into(),
            expected_revision: Some(session.revision),
        })
        .await
        .expect("fenced follow-up on the current revision");
    assert!(accepted.revision.is_newer_than(session.revision));

    // The same fence is now stale.
    let error = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "second follow-up".into(),
            expected_revision: Some(session.revision),
        })
        .await
        .expect_err("stale fence must fail closed");
    assert_eq!(error.code, SdkErrorCode::StaleVersion);
    assert_eq!(error.detail("currentRevision"), Some("2"));

    // Chaining from the receipt's revision works.
    plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(3),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "third follow-up".into(),
            expected_revision: Some(accepted.revision),
        })
        .await
        .expect("chaining from the receipt revision");
}

// ── Lost connection ───────────────────────────────────────────────────────

#[tokio::test]
async fn a_dropped_connection_is_typed_and_safely_retryable() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");

    plane.inject_for(Operation::StreamEvents, Fault::LostConnection);
    let error = plane
        .stream_events(selector(&session, &accepted.run_id), PageRequest::new())
        .await
        .expect_err("armed lost connection");

    assert_eq!(error.code, SdkErrorCode::TransportUnavailable);
    assert_eq!(error.code.origin(), ErrorOrigin::Seam);
    assert!(error.code.is_safely_retryable());

    // Reconnecting resumes; nothing about the run changed.
    let page = plane
        .stream_events(selector(&session, &accepted.run_id), PageRequest::new())
        .await
        .expect("resume after reconnect");
    assert!(page.is_caught_up());
}

#[tokio::test]
async fn a_reconnecting_reader_resumes_from_its_cursor_without_duplicates() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");

    let mut seen: Vec<Cursor> = Vec::new();
    let mut request = PageRequest::new().limit(2);
    loop {
        let page = plane
            .stream_events(selector(&session, &accepted.run_id), request.clone())
            .await
            .expect("page");
        seen.extend(page.items.iter().map(|e| e.cursor.clone()));
        match page.next_cursor {
            None => break,
            Some(cursor) => {
                // Simulate a drop between pages; the cursor is all we keep.
                plane.inject_for(Operation::StreamEvents, Fault::LostConnection);
                let _ = plane
                    .stream_events(selector(&session, &accepted.run_id), PageRequest::new())
                    .await;
                request = PageRequest::new().after(cursor).limit(2);
            }
        }
    }

    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        seen.len(),
        "resume duplicated events: {seen:?}"
    );
    assert!(seen.len() >= 6, "expected the full journal, got {seen:?}");
}

#[tokio::test]
async fn an_expired_cursor_reports_the_still_readable_range() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");

    plane.expire_events_through(&accepted.run_id, 3);
    let error = plane
        .stream_events(
            selector(&session, &accepted.run_id),
            PageRequest::new().after(Cursor::from_opaque("1")),
        )
        .await
        .expect_err("cursor below the retained window");

    assert_eq!(error.code, SdkErrorCode::CursorExpired);
    let start = error.detail("retainedStart").expect("retained start");
    let end = error.detail("retainedEnd").expect("retained end");
    assert_eq!(start, "4");
    assert!(end.parse::<u64>().expect("numeric end") >= 4);

    // Resuming from the reported start works without a second read.
    let page = plane
        .stream_events(
            selector(&session, &accepted.run_id),
            PageRequest::new().after(Cursor::from_opaque("3")),
        )
        .await
        .expect("resume at the retained start");
    assert!(!page.items.is_empty());
}

// ── Cancellation ──────────────────────────────────────────────────────────

#[tokio::test]
async fn cancelling_a_queued_run_never_launches_a_turn_and_is_idempotent() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");

    let request = CancelRequest {
        request_id: request_id(2),
        selector: selector(&session, &accepted.run_id),
    };
    let first = plane.cancel_run(request.clone()).await.expect("cancel");
    assert_eq!(first.lifecycle, RunLifecycle::Cancelled);
    assert!(first.was_queued);
    assert!(!first.replayed);

    let second = plane.cancel_run(request).await.expect("cancel replay");
    assert_eq!(second.lifecycle, RunLifecycle::Cancelled);
    assert_eq!(second.revision, first.revision, "replay must not re-mutate");
    assert!(second.replayed);

    // A cancelled run is terminal and cannot be started.
    assert!(plane.start_run(&accepted.run_id).is_err());
}

#[tokio::test]
async fn follow_up_does_not_cancel_an_active_turn() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");

    let receipt = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "also check the README".into(),
            expected_revision: None,
        })
        .await
        .expect("follow-up");
    assert_eq!(receipt.disposition, FollowUpDisposition::Pending);

    let view = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    assert_eq!(view.lifecycle, RunLifecycle::Running);
}

#[tokio::test]
async fn follow_up_to_an_idle_session_is_queued_not_pending() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);

    let receipt = plane
        .request_follow_up(FollowUpRequest {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "queued work".into(),
            expected_revision: None,
        })
        .await
        .expect("follow-up");
    assert_eq!(receipt.disposition, FollowUpDisposition::Queued);
}

// ── Uncertain send ────────────────────────────────────────────────────────

#[tokio::test]
async fn an_uncertain_send_is_never_classified_as_retryable() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let key = request_id(1);

    plane.inject_for(Operation::RequestFollowUp, Fault::UncertainSend);
    let error = plane
        .request_follow_up(FollowUpRequest {
            request_id: key.clone(),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "did this land?".into(),
            expected_revision: None,
        })
        .await
        .expect_err("armed uncertain send");

    assert_eq!(error.code, SdkErrorCode::UncertainOutcome);
    assert_eq!(error.retry_disposition(), RetryDisposition::Unsafe);
    assert!(!error.code.is_safely_retryable());
    assert_eq!(
        recover_mutation(&key, &error),
        MutationRecovery::ReconcileFirst,
        "an uncertain mutation must be reconciled, never auto-retried"
    );
}

// ── Authorization and cross-tenant denial ─────────────────────────────────

#[tokio::test]
async fn an_invalid_credential_is_unauthenticated_and_not_retryable() {
    let plane = FakeControlPlane::builder().build();
    plane.inject(Fault::Unauthenticated);

    let error = plane.capabilities().await.expect_err("armed auth failure");
    assert_eq!(error.code, SdkErrorCode::Unauthenticated);
    assert_eq!(error.code.origin(), ErrorOrigin::Runtime);
    assert_eq!(error.retry_disposition(), RetryDisposition::Never);
}

#[tokio::test]
async fn a_non_allowlisted_workspace_fails_before_any_session_lookup() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let foreign = WorkspaceRef::new("ws-not-served").expect("valid ref");

    let error = plane
        .observe_run(RunSelector {
            session_id: session.session_id.clone(),
            workspace: foreign.clone(),
            run_id: RunId::new("run-0001").expect("valid id"),
        })
        .await
        .expect_err("non-allowlisted workspace");

    // Session-independent: this must not depend on the run existing.
    assert_eq!(error.code, SdkErrorCode::WorkspaceMismatch);

    let create = plane
        .create_session(CreateSessionRequest {
            request_id: request_id(9),
            workspace: foreign,
            title: None,
        })
        .await
        .expect_err("cannot create on a non-allowlisted workspace");
    assert_eq!(create.code, SdkErrorCode::WorkspaceMismatch);
}

#[tokio::test]
async fn unknown_cross_session_and_cross_tenant_reads_are_indistinguishable() {
    let plane = FakeControlPlane::builder()
        .workspace("ws-beta", "beta")
        .build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");

    // 1. A run that does not exist.
    let unknown = plane
        .observe_run(RunSelector {
            run_id: RunId::new("run-9999").expect("valid id"),
            ..selector(&session, &accepted.run_id)
        })
        .await
        .expect_err("unknown run");

    // 2. A real run, claimed through another session in the same tenant.
    let other_session = plane
        .create_session(CreateSessionRequest {
            request_id: request_id(2),
            workspace: session.workspace.clone(),
            title: None,
        })
        .await
        .expect("second session");
    let cross_session = plane
        .observe_run(RunSelector {
            session_id: other_session.session_id.clone(),
            workspace: session.workspace.clone(),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect_err("cross-session read");

    // 3. A real run, claimed through an allowlisted but different workspace.
    let cross_workspace = plane
        .observe_run(RunSelector {
            session_id: session.session_id.clone(),
            workspace: WorkspaceRef::new("ws-beta").expect("valid ref"),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect_err("cross-workspace read");

    for error in [&unknown, &cross_session, &cross_workspace] {
        assert_eq!(error.code, SdkErrorCode::ForbiddenScope);
    }
    // Byte-identical, so no read is an existence oracle for another scope.
    assert_eq!(unknown.message, cross_session.message);
    assert_eq!(unknown.message, cross_workspace.message);
    assert_eq!(unknown.details, cross_session.details);
}

#[tokio::test]
async fn a_cross_tenant_session_is_invisible_to_another_owner() {
    let acme = FakeControlPlane::builder().owner("acme").build();
    let globex = FakeControlPlane::builder().owner("globex").build();

    let acme_session = session_of(&acme);
    let globex_session = session_of(&globex);
    assert_eq!(acme_session.session_id, globex_session.session_id);

    let accepted = acme
        .submit_task(submission(&acme_session, 1, "acme work"))
        .await
        .expect("submit");

    // The same identifiers, presented to the other tenant's host.
    let error = globex
        .observe_run(RunSelector {
            session_id: acme_session.session_id.clone(),
            workspace: acme_session.workspace.clone(),
            run_id: accepted.run_id.clone(),
        })
        .await
        .expect_err("cross-tenant read must fail closed");
    assert_eq!(error.code, SdkErrorCode::ForbiddenScope);

    let sessions = globex
        .list_sessions(PageRequest::new())
        .await
        .expect("list");
    assert!(sessions
        .items
        .iter()
        .all(|s| s.session_id == globex_session.session_id));
}

// ── Capability discovery and denial rules ─────────────────────────────────

#[tokio::test]
async fn computer_use_control_and_provider_credentials_are_always_forbidden() {
    let plane = FakeControlPlane::builder()
        .capabilities(
            [
                CapabilityId::TaskSubmit,
                // An adapter trying to advertise the forbidden pair.
                CapabilityId::ComputerControl,
                CapabilityId::ProviderCredentials,
            ]
            .into_iter()
            .map(|id| CapabilityDescriptor {
                id,
                since: ContractVersion::new(1, 0),
                availability: Availability::Available,
            })
            .collect(),
        )
        .build();

    let connected = plane.connect().await.expect("connect");
    for id in CapabilityId::permanently_forbidden() {
        let error = connected
            .require(id)
            .expect_err("permanently forbidden capability");
        assert_eq!(error.code, SdkErrorCode::ForbiddenScope);
        assert_eq!(error.detail("capability"), Some(id.as_wire()));
    }
    assert!(connected.require(&CapabilityId::TaskSubmit).is_ok());
}

#[tokio::test]
async fn an_unsupported_capability_is_distinct_from_an_unadvertised_one() {
    let plane = FakeControlPlane::builder()
        .capabilities(vec![CapabilityDescriptor {
            id: CapabilityId::ComputerRead,
            since: ContractVersion::new(1, 0),
            availability: Availability::Unsupported {
                reason: Label::new("no computer-use ledger on this host").expect("label"),
            },
        }])
        .build();
    let connected = plane.connect().await.expect("connect");

    assert_eq!(
        connected
            .require(&CapabilityId::ComputerRead)
            .expect_err("unsupported")
            .code,
        SdkErrorCode::Unsupported
    );
    assert_eq!(
        connected
            .require(&CapabilityId::ArtifactFetch)
            .expect_err("unadvertised")
            .code,
        SdkErrorCode::CapabilityUnavailable
    );
}

// ── Artifacts ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn artifacts_are_digest_verified_before_a_consumer_sees_them() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");

    let view = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    let descriptor = view.artifacts.first().cloned().expect("one artifact");

    let payload = plane
        .fetch_artifact(ArtifactRequest {
            selector: selector(&session, &accepted.run_id),
            artifact_id: descriptor.artifact_id.clone(),
            max_bytes: None,
        })
        .await
        .expect("fetch");
    payload
        .verify(descriptor.byte_len)
        .expect("adapter returned a verifiable artifact");
    assert_eq!(payload.descriptor.media, ArtifactMedia::UnifiedDiff);

    // Tamper with the stored body; the digest no longer matches.
    plane.corrupt_artifact(
        &accepted.run_id,
        &descriptor.artifact_id,
        "not the reviewed diff",
    );
    let error = plane
        .fetch_artifact(ArtifactRequest {
            selector: selector(&session, &accepted.run_id),
            artifact_id: descriptor.artifact_id.clone(),
            max_bytes: None,
        })
        .await
        .expect_err("corrupted artifact must not be returned");
    assert_eq!(error.code, SdkErrorCode::IntegrityMismatch);
}

#[tokio::test]
async fn an_artifact_over_the_caller_ceiling_is_refused() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let accepted = plane
        .submit_task(submission(&session, 1, "instruction"))
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");
    let view = plane
        .observe_run(selector(&session, &accepted.run_id))
        .await
        .expect("observe");
    let descriptor = view.artifacts.first().cloned().expect("one artifact");

    let error = plane
        .fetch_artifact(ArtifactRequest {
            selector: selector(&session, &accepted.run_id),
            artifact_id: descriptor.artifact_id,
            max_bytes: Some(4),
        })
        .await
        .expect_err("over-ceiling fetch");
    assert_eq!(error.code, SdkErrorCode::InvalidRequest);
    assert_eq!(error.detail("maxBytes"), Some("4"));
}

// ── Lease / acquire control ───────────────────────────────────────────────

#[tokio::test]
async fn a_lease_is_exclusive_idempotent_and_never_serializes_its_secret() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);
    let work_id = WorkId::new("work-0001").expect("valid id");
    let claimant = AgentId::new("agent-0001").expect("valid id");

    let request = ControlLeaseRequest {
        request_id: request_id(1),
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        work_id: work_id.clone(),
        claimant: claimant.clone(),
        requested_ttl_ms: Some(30_000),
    };
    let lease = plane.acquire_control(request.clone()).await.expect("claim");
    assert_eq!(lease.claimant, claimant);
    assert!(!lease.credential.is_empty());

    let secret = lease.credential.reveal().to_string();
    assert!(!format!("{lease:?}").contains(&secret));
    assert!(!serde_json::to_string(&lease)
        .expect("serialize lease")
        .contains(&secret));

    // Exact replay returns the same attempt, not a second one.
    let replay = plane.acquire_control(request).await.expect("replay");
    assert_eq!(replay.attempt_id, lease.attempt_id);

    // A second claimant is a conflict, not a takeover.
    let conflict = plane
        .acquire_control(ControlLeaseRequest {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id: work_id.clone(),
            claimant: AgentId::new("agent-0002").expect("valid id"),
            requested_ttl_ms: None,
        })
        .await
        .expect_err("second claimant");
    assert_eq!(conflict.code, SdkErrorCode::Conflict);

    // Release, then the work item is claimable again.
    plane
        .release_control(ReleaseLeaseRequest {
            request_id: request_id(3),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id: work_id.clone(),
            attempt_id: lease.attempt_id.clone(),
        })
        .await
        .expect("release");
    plane
        .acquire_control(ControlLeaseRequest {
            request_id: request_id(4),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            work_id,
            claimant: AgentId::new("agent-0002").expect("valid id"),
            requested_ttl_ms: None,
        })
        .await
        .expect("reclaim after release");
}

#[tokio::test]
async fn a_lease_cannot_be_acquired_across_a_workspace_boundary() {
    let plane = FakeControlPlane::builder().build();
    let session = session_of(&plane);

    let error = plane
        .acquire_control(ControlLeaseRequest {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: WorkspaceRef::new("ws-not-served").expect("valid ref"),
            work_id: WorkId::new("work-0001").expect("valid id"),
            claimant: AgentId::new("agent-0001").expect("valid id"),
            requested_ttl_ms: None,
        })
        .await
        .expect_err("cross-workspace claim");
    assert_eq!(error.code, SdkErrorCode::WorkspaceMismatch);
}

// ── Version negotiation ───────────────────────────────────────────────────

#[tokio::test]
async fn an_older_host_minor_degrades_instead_of_failing() {
    let plane = FakeControlPlane::builder()
        .contract_version(ContractVersion::new(CONTRACT_VERSION.major, 0))
        .capabilities(vec![
            CapabilityDescriptor {
                id: CapabilityId::TaskSubmit,
                since: ContractVersion::new(CONTRACT_VERSION.major, 0),
                availability: Availability::Available,
            },
            CapabilityDescriptor {
                id: CapabilityId::RunEventsLive,
                // Introduced above what this host serves.
                since: ContractVersion::new(CONTRACT_VERSION.major, 5),
                availability: Availability::Available,
            },
        ])
        .build();

    let connected = plane.connect().await.expect("connect");
    assert_eq!(
        connected.negotiated.effective,
        ContractVersion::new(CONTRACT_VERSION.major, 0)
    );
    // A capability at or below the negotiated minor still works.
    assert!(connected.require(&CapabilityId::TaskSubmit).is_ok());
    // One above it is refused with a version error, not a scope error.
    let error = connected
        .require(&CapabilityId::RunEventsLive)
        .expect_err("above the negotiated minor");
    assert_eq!(error.code, SdkErrorCode::ContractVersionUnsupported);
    assert_eq!(
        error.detail("negotiatedContractVersion"),
        Some(format!("{}.0", CONTRACT_VERSION.major).as_str())
    );
}

#[tokio::test]
async fn a_host_on_a_different_major_is_refused_rather_than_guessed_at() {
    let plane = FakeControlPlane::builder()
        .contract_version(ContractVersion::new(CONTRACT_VERSION.major + 1, 0))
        .build();

    let error = plane.connect().await.expect_err("major mismatch");
    assert_eq!(error.code, SdkErrorCode::ContractVersionUnsupported);
    assert_eq!(error.code.origin(), ErrorOrigin::Seam);
}

#[tokio::test]
async fn an_unknown_wire_code_from_a_newer_host_does_not_break_decoding() {
    // A future host adds an error code and a capability this build never saw.
    let raw = serde_json::json!({
        "code": "quota_exhausted_for_org",
        "message": "future runtime code",
    });
    let error: SdkError = serde_json::from_value(raw).expect("decode a future error");
    assert_eq!(error.code.as_wire(), "quota_exhausted_for_org");
    assert_eq!(error.code.origin(), ErrorOrigin::Unrecognized);
    // Unknown codes are conservatively non-retryable.
    assert_eq!(error.retry_disposition(), RetryDisposition::Never);

    let capability: CapabilityId =
        serde_json::from_value(serde_json::json!("workload.federate")).expect("decode");
    assert_eq!(capability.as_wire(), "workload.federate");
    assert!(!capability.is_permanently_forbidden());
}

#[tokio::test]
async fn hostile_identifiers_are_rejected_on_decode_not_just_on_construction() {
    // A host that returns a traversal path or a control-character label must
    // not be able to smuggle it through JSON.
    let bad_path = serde_json::json!({
        "path": "../../etc/passwd",
        "summary": "escaped"
    });
    assert!(serde_json::from_value::<ChangedFile>(bad_path).is_err());

    let absolute = serde_json::json!({
        "path": "/etc/passwd",
        "summary": "escaped"
    });
    assert!(serde_json::from_value::<ChangedFile>(absolute).is_err());

    let path_shaped_ref = serde_json::json!("/home/user/project");
    assert!(serde_json::from_value::<WorkspaceRef>(path_shaped_ref).is_err());

    // Control characters in a bounded summary are stripped, not preserved.
    let escaped = serde_json::json!({
        "path": "src/lib.rs",
        "summary": "line\u{1b}[31mred"
    });
    let changed: ChangedFile = serde_json::from_value(escaped).expect("decode");
    assert!(!changed.summary.as_str().contains('\u{1b}'));
}

// ── The battery, run against the fake ─────────────────────────────────────

struct FakeHarness {
    plane: FakeControlPlane,
    next: AtomicU64,
}

#[async_trait::async_trait]
impl Harness for FakeHarness {
    fn plane(&self) -> &dyn AgentControlPlane {
        &self.plane
    }

    async fn owned_session(&self) -> SessionView {
        self.plane.seeded_session().expect("seeded session")
    }

    async fn foreign_workspace(&self) -> Option<WorkspaceRef> {
        WorkspaceRef::new("ws-not-served").ok()
    }

    async fn cross_tenant_session(&self) -> Option<SessionView> {
        // A real session on this same host, owned by another account.
        self.plane.foreign_session()
    }

    async fn arm_lost_connection(&self) -> bool {
        self.plane.inject(Fault::LostConnection);
        true
    }

    async fn arm_uncertain_send(&self) -> bool {
        self.plane
            .inject_for(Operation::RequestFollowUp, Fault::UncertainSend);
        true
    }

    async fn drive_to_completion(&self, run_id: &RunId) -> bool {
        self.plane.start_run(run_id).is_ok()
            && self
                .plane
                .finish_run(run_id, ScriptedOutcome::Completed)
                .is_ok()
    }

    async fn expire_early_events(&self, run_id: &RunId) -> bool {
        self.plane.expire_events_through(run_id, 3);
        true
    }

    async fn corrupt_artifact(&self, run_id: &RunId, artifact_id: &ArtifactId) -> bool {
        self.plane
            .corrupt_artifact(run_id, artifact_id, "tampered body");
        true
    }

    fn next_request_id(&self) -> RequestId {
        let n = self.next.fetch_add(1, Ordering::SeqCst);
        RequestId::new(format!("battery-{n:04}")).expect("minted id is valid")
    }
}

#[tokio::test]
async fn the_conformance_battery_passes_against_the_fake_with_no_skips() {
    let harness = FakeHarness {
        plane: FakeControlPlane::builder()
            .workspace("ws-beta", "beta")
            .build(),
        next: AtomicU64::new(1),
    };

    let report = conformance::run_battery(&harness).await;
    assert!(report.is_clean(), "{}", report.summary());
    // The fake can produce every fault, so nothing may be skipped here. A real
    // adapter is allowed to skip; the fake is what proves the checks run.
    let skipped: Vec<&str> = report.skipped().map(|c| c.name).collect();
    assert!(skipped.is_empty(), "unexpected skips: {skipped:?}");
    assert!(report.passed_count() >= 18, "{}", report.summary());
}

#[tokio::test]
async fn the_battery_reports_a_skip_rather_than_a_pass_when_a_fault_is_unavailable() {
    struct Limited(FakeHarness);

    #[async_trait::async_trait]
    impl Harness for Limited {
        fn plane(&self) -> &dyn AgentControlPlane {
            self.0.plane()
        }
        async fn owned_session(&self) -> SessionView {
            self.0.owned_session().await
        }
        async fn drive_to_completion(&self, run_id: &RunId) -> bool {
            self.0.drive_to_completion(run_id).await
        }
        fn next_request_id(&self) -> RequestId {
            self.0.next_request_id()
        }
        // Every optional capability keeps its default: unavailable.
    }

    let harness = Limited(FakeHarness {
        plane: FakeControlPlane::builder().build(),
        next: AtomicU64::new(1),
    });
    let report = conformance::run_battery(&harness).await;
    assert!(report.is_clean(), "{}", report.summary());

    let skipped: Vec<&str> = report.skipped().map(|c| c.name).collect();
    for expected in [
        "faults.lost_connection_is_safely_retryable",
        "faults.uncertain_send_is_never_auto_retried",
        "authz.foreign_workspace_is_workspace_mismatch",
        "authz.cross_tenant_read_is_indistinguishable",
        "events.expired_cursor_reports_retained_range",
        "artifacts.digest_mismatch_is_integrity_error",
    ] {
        assert!(
            skipped.contains(&expected),
            "expected a skip for {expected}"
        );
    }
    assert!(report
        .checks
        .iter()
        .all(|c| c.outcome != CheckOutcome::Failed(String::new())));
}
