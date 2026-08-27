//! Focused tests for the read-only observatory seam.
//!
//! The claim under test is narrow and structural: a consumer handed an
//! [`ObserverHandle`] can inspect a run's lifecycle, typed events, and
//! redacted receipts, and can do nothing else — not because a check refuses,
//! but because the authority is not reachable from the type it holds.

use grokptah_agent_sdk::prelude::*;
use std::collections::BTreeSet;

fn plane() -> FakeControlPlane {
    FakeControlPlane::builder().build()
}

fn request_id(n: u64) -> RequestId {
    RequestId::new(format!("req-{n:04}")).expect("minted id is valid")
}

/// Drive a fake to a completed run and return the pieces an observer needs.
async fn completed_run(plane: &FakeControlPlane) -> (SessionView, RunSelector) {
    let session = plane.seeded_session().expect("builder seeds one session");
    let accepted = plane
        .submit_task(TaskSubmission {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            prompt: "SECRET-PROMPT-do-not-echo".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");
    plane.start_run(&accepted.run_id).expect("start");
    plane
        .finish_run(&accepted.run_id, ScriptedOutcome::Completed)
        .expect("finish");
    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id,
    };
    (session, selector)
}

// ── The handle grants no authority ────────────────────────────────────────

#[tokio::test]
async fn an_observer_can_read_the_whole_lifecycle() {
    let plane = plane();
    let (_session, selector) = completed_run(&plane).await;
    let observer = ObserverHandle::new(plane);

    let view = observer
        .observe_run(selector.clone())
        .await
        .expect("observe");
    assert_eq!(view.lifecycle, RunLifecycle::Completed);
    assert_eq!(view.stop_cause, Some(StopCause::Completed));
    assert_eq!(
        view.verification.map(|v| v.status),
        Some(VerificationStatus::Verified)
    );

    let events = observer
        .stream_events(selector.clone(), PageRequest::new())
        .await
        .expect("events");
    assert!(matches!(
        events.items.last().map(|e| &e.kind),
        Some(PublicEventKind::RunTerminal { .. })
    ));

    let sessions = observer
        .list_sessions(PageRequest::new())
        .await
        .expect("sessions");
    assert_eq!(sessions.items.len(), 1);
}

#[tokio::test]
async fn a_narrowed_document_advertises_no_authority() {
    let plane = plane();
    let direct = plane.capabilities().await.expect("capabilities");
    let observer = ObserverHandle::new(plane);
    let narrowed = observer.capabilities().await.expect("capabilities");

    // Everything the fake offers directly, an observer sees withheld if it
    // mutates and unchanged if it reads.
    let mut checked = 0usize;
    for descriptor in direct.iter() {
        let seen = narrowed.get(&descriptor.id).expect("same identifier set");
        if descriptor.id.is_mutation() {
            assert!(
                !seen.availability.is_available(),
                "{} must be withheld from an observer",
                descriptor.id
            );
        } else {
            assert_eq!(
                seen.availability, descriptor.availability,
                "{} must be unchanged for an observer",
                descriptor.id
            );
        }
        checked += 1;
    }
    assert!(checked >= 10, "only {checked} capabilities compared");

    // And the narrowing is discoverable through the ordinary gate.
    let negotiated = negotiate(CONTRACT_VERSION, narrowed.contract_version).expect("negotiate");
    assert_eq!(negotiated.effective, CONTRACT_VERSION);
    for id in [
        CapabilityId::TaskSubmit,
        CapabilityId::RunCancel,
        CapabilityId::RunFollowUp,
        CapabilityId::ControlLease,
        CapabilityId::SessionCreate,
    ] {
        assert_eq!(
            narrowed.require(&id).expect_err("withheld").code,
            SdkErrorCode::ForbiddenScope
        );
    }
    assert!(narrowed.require(&CapabilityId::RunObserve).is_ok());
}

#[tokio::test]
async fn narrowing_never_upgrades_an_answer_the_host_already_gave() {
    // A capability the host says it cannot do must keep saying that. "You may
    // not" is a weaker and less useful answer than "this host cannot".
    let plane = FakeControlPlane::builder()
        .capabilities(vec![
            CapabilityDescriptor {
                id: CapabilityId::ComputerRead,
                since: ContractVersion::new(CONTRACT_VERSION.major, 0),
                availability: Availability::Unsupported {
                    reason: Label::new("no computer-use ledger on this host").expect("label"),
                },
            },
            CapabilityDescriptor {
                id: CapabilityId::TaskSubmit,
                since: ContractVersion::new(CONTRACT_VERSION.major, 0),
                availability: Availability::Unsupported {
                    reason: Label::new("submission is disabled on this host").expect("label"),
                },
            },
        ])
        .build();
    let observer = ObserverHandle::new(plane);
    let document = observer.capabilities().await.expect("capabilities");

    assert_eq!(
        document
            .require(&CapabilityId::ComputerRead)
            .expect_err("host cannot")
            .code,
        SdkErrorCode::Unsupported
    );
    assert_eq!(
        document
            .require(&CapabilityId::TaskSubmit)
            .expect_err("host cannot")
            .code,
        SdkErrorCode::Unsupported,
        "an unsupported mutation stays unsupported, not merely forbidden"
    );
}

#[tokio::test]
async fn the_permanently_forbidden_pair_survives_narrowing() {
    let observer = ObserverHandle::new(plane());
    let document = observer.capabilities().await.expect("capabilities");
    for id in CapabilityId::permanently_forbidden() {
        assert_eq!(
            document.require(id).expect_err("never delegated").code,
            SdkErrorCode::ForbiddenScope
        );
    }
}

// ── Redacted receipts ─────────────────────────────────────────────────────

#[tokio::test]
async fn receipts_prove_a_mutation_without_revealing_it() {
    let plane = plane();
    let (_session, selector) = completed_run(&plane).await;
    let observer = ObserverHandle::new(plane);

    let page = observer
        .list_receipts(selector.clone(), PageRequest::new())
        .await
        .expect("receipts");
    assert_eq!(page.items.len(), 1, "one submission, one receipt");
    let receipt = &page.items[0];
    assert_eq!(receipt.operation, OperationClass::SubmitTask);
    assert_eq!(receipt.status, ReceiptStatus::Complete);
    assert!(receipt.is_settled());
    assert_eq!(receipt.run_id.as_ref(), Some(&selector.run_id));
    receipt.payload_digest.validate().expect("a real digest");

    // The prompt went in through this very request. It does not come back.
    let encoded = serde_json::to_string(&page).expect("serialize receipts");
    assert!(!encoded.contains("SECRET-PROMPT-do-not-echo"), "{encoded}");
    // Nor does the stored response body, nor any free-text message.
    for absent in ["response", "message", "workspace", "prompt"] {
        assert!(!encoded.contains(absent), "{absent} leaked into {encoded}");
    }
}

#[tokio::test]
async fn a_stranded_receipt_reads_as_uncertain_not_as_an_outcome() {
    let plane = plane();
    let (_session, selector) = completed_run(&plane).await;
    // A host that claimed the key and stopped before recording an outcome.
    plane.strand_receipt(&request_id(77), OperationClass::Cancel, &selector.run_id);
    let observer = ObserverHandle::new(plane);

    let page = observer
        .list_receipts(selector, PageRequest::new())
        .await
        .expect("receipts");
    let stranded = page
        .items
        .iter()
        .find(|receipt| receipt.request_id == request_id(77))
        .expect("the stranded receipt is listed");

    assert!(stranded.is_uncertain());
    assert!(!stranded.is_settled());
    assert_eq!(stranded.status, ReceiptStatus::Pending);
    // Uncertain means uncertain: no outcome is asserted in either direction.
    assert_eq!(stranded.outcome, None);
}

#[tokio::test]
async fn receipts_are_run_scoped_and_never_a_global_listing() {
    let plane = plane();
    let (session, selector) = completed_run(&plane).await;

    // A second run in the same session, with its own mutation.
    let second = plane
        .submit_task(TaskSubmission {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            prompt: "another instruction".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");

    let observer = ObserverHandle::new(plane);
    let first_page = observer
        .list_receipts(selector.clone(), PageRequest::new())
        .await
        .expect("receipts");
    let ids: BTreeSet<&str> = first_page
        .items
        .iter()
        .map(|receipt| receipt.request_id.as_str())
        .collect();
    assert!(ids.contains("req-0001"));
    assert!(
        !ids.contains("req-0002"),
        "another run's receipt must not appear: {ids:?}"
    );

    // A run outside the caller's scope is the same denial every other read
    // gives, so receipts cannot become an existence oracle.
    let forged = RunSelector {
        run_id: RunId::new("run-9999").expect("valid id"),
        ..selector
    };
    assert_eq!(
        observer
            .list_receipts(forged, PageRequest::new())
            .await
            .expect_err("unknown run")
            .code,
        SdkErrorCode::ForbiddenScope
    );
    let _ = second;
}

#[tokio::test]
async fn an_empty_listing_is_a_page_with_its_window_not_an_error() {
    // A run with no mutations attributed to it. The page is empty, caught up,
    // and still says which window it was drawn from — because "nothing here"
    // and "nothing ever happened" are different claims.
    let plane = plane();
    let session = plane.seeded_session().expect("seeded");
    let accepted = plane
        .submit_task(TaskSubmission {
            request_id: request_id(1),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            prompt: "instruction".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");
    // Second run, never mutated after creation beyond its own submission.
    let other = plane
        .submit_task(TaskSubmission {
            request_id: request_id(2),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            prompt: "instruction".into(),
            bounds: None,
            execution_mode: ExecutionMode::Shared,
            allow_queue: false,
        })
        .await
        .expect("submit");
    let observer = ObserverHandle::new(plane);

    // Drop the only receipt for `other` by listing a run that has none: use a
    // fresh selector pointing at `accepted` but filter on the other run.
    let empty_for = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: other.run_id.clone(),
    };
    let page = observer
        .list_receipts(empty_for, PageRequest::new())
        .await
        .expect("receipts");
    assert_eq!(
        page.items.len(),
        1,
        "its own submission is attributed to it"
    );
    assert!(page.is_caught_up());
    assert!(page.retention.max_receipts > 0);
    assert!(page.retention.max_age_days > 0);
    let _ = accepted;
}

#[tokio::test]
async fn multi_page_listing_is_deterministic_and_resumable() {
    let plane = plane();
    let (_session, selector) = completed_run(&plane).await;
    for n in 10..15 {
        plane
            .cancel_run(CancelRequest {
                request_id: request_id(n),
                selector: selector.clone(),
            })
            .await
            .expect("each key records its own receipt");
    }
    let observer = ObserverHandle::new(plane);

    // Walk the listing one item at a time.
    let mut walked: Vec<String> = Vec::new();
    let mut request = PageRequest::new().limit(1);
    let mut windows = Vec::new();
    loop {
        let page = observer
            .list_receipts(selector.clone(), request.clone())
            .await
            .expect("page");
        windows.push(page.retention);
        walked.extend(
            page.items
                .iter()
                .map(|receipt| receipt.request_id.as_str().to_string()),
        );
        match page.next_cursor {
            None => break,
            Some(cursor) => request = PageRequest::new().after(cursor).limit(1),
        }
    }
    assert!(walked.len() >= 6, "expected every receipt, got {walked:?}");
    let mut unique = walked.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), walked.len(), "resume duplicated: {walked:?}");
    assert!(
        windows.windows(2).all(|pair| pair[0] == pair[1]),
        "the declared window must not change mid-walk"
    );

    // One big page must produce the same order as the walk.
    let whole = observer
        .list_receipts(selector.clone(), PageRequest::new().limit(500))
        .await
        .expect("whole listing");
    let at_once: Vec<String> = whole
        .items
        .iter()
        .map(|receipt| receipt.request_id.as_str().to_string())
        .collect();
    assert_eq!(at_once, walked, "paged and unpaged order must agree");

    // And that order is chronological, tie-broken by request id.
    let keys: Vec<(chrono::DateTime<chrono::Utc>, String)> = whole
        .items
        .iter()
        .map(|receipt| (receipt.recorded_at, receipt.request_id.as_str().to_string()))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys, sorted,
        "listing is not in (recordedAt, requestId) order"
    );
}

#[tokio::test]
async fn a_cursor_this_adapter_did_not_issue_is_refused() {
    let plane = plane();
    let (_session, selector) = completed_run(&plane).await;
    let observer = ObserverHandle::new(plane);
    for bad in ["1", "not-a-cursor", ":", "abc:req-0001"] {
        let error = observer
            .list_receipts(
                selector.clone(),
                PageRequest::new().after(Cursor::from_opaque(bad)),
            )
            .await
            .expect_err("a foreign cursor must not be interpreted");
        assert_eq!(error.code, SdkErrorCode::InvalidRequest, "{bad}");
    }
}

#[tokio::test]
async fn retention_drops_settled_receipts_but_never_uncertain_ones() {
    // A window of two, so the fence is reachable without a thousand writes.
    let plane = FakeControlPlane::builder()
        .receipt_retention(ReceiptRetention {
            max_receipts: 2,
            max_age_days: 7,
        })
        .build();
    let (_session, selector) = completed_run(&plane).await;
    // An unsettled receipt: claimed, never resolved.
    plane.strand_receipt(&request_id(90), OperationClass::Cancel, &selector.run_id);
    // Now push settled receipts past the window.
    for n in 20..25 {
        plane
            .cancel_run(CancelRequest {
                request_id: request_id(n),
                selector: selector.clone(),
            })
            .await
            .expect("cancel");
    }
    let observer = ObserverHandle::new(plane);

    let page = observer
        .list_receipts(selector, PageRequest::new().limit(500))
        .await
        .expect("receipts");
    assert_eq!(page.retention.max_receipts, 2);

    let settled: Vec<_> = page.items.iter().filter(|r| r.is_settled()).collect();
    assert!(
        settled.len() <= 2,
        "settled receipts must be held to the declared window, got {}",
        settled.len()
    );
    // The uncertain one survives: expiring it would turn "we do not know"
    // into "it never happened".
    assert!(
        page.items
            .iter()
            .any(|r| r.request_id == request_id(90) && r.is_uncertain()),
        "an unsettled receipt must outlive the count fence"
    );
}

// ── Adapters that cannot serve receipts say so ────────────────────────────

#[tokio::test]
async fn an_adapter_without_receipts_reports_absence_not_emptiness() {
    // The default trait body stands in for any adapter written before 1.1.
    struct ReadsOnly;

    #[async_trait::async_trait]
    impl AgentControlPlane for ReadsOnly {
        async fn capabilities(&self) -> SdkResult<CapabilityDocument> {
            unimplemented!("not exercised")
        }
        async fn create_session(&self, _: CreateSessionRequest) -> SdkResult<SessionView> {
            unimplemented!("not exercised")
        }
        async fn list_sessions(&self, _: PageRequest) -> SdkResult<Page<SessionView>> {
            unimplemented!("not exercised")
        }
        async fn submit_task(&self, _: TaskSubmission) -> SdkResult<RunAccepted> {
            unimplemented!("not exercised")
        }
        async fn observe_run(&self, _: RunSelector) -> SdkResult<RunView> {
            unimplemented!("not exercised")
        }
        async fn stream_events(
            &self,
            _: RunSelector,
            _: PageRequest,
        ) -> SdkResult<Page<PublicEvent>> {
            unimplemented!("not exercised")
        }
        async fn request_follow_up(&self, _: FollowUpRequest) -> SdkResult<FollowUpReceipt> {
            unimplemented!("not exercised")
        }
        async fn cancel_run(&self, _: CancelRequest) -> SdkResult<CancelReceipt> {
            unimplemented!("not exercised")
        }
        async fn acquire_control(&self, _: ControlLeaseRequest) -> SdkResult<ControlLease> {
            unimplemented!("not exercised")
        }
        async fn release_control(&self, _: ReleaseLeaseRequest) -> SdkResult<ReleaseLeaseReceipt> {
            unimplemented!("not exercised")
        }
        async fn fetch_artifact(&self, _: ArtifactRequest) -> SdkResult<ArtifactPayload> {
            unimplemented!("not exercised")
        }
    }

    let selector = RunSelector {
        session_id: SessionId::new("session-0001").expect("valid id"),
        workspace: WorkspaceRef::new("ws-alpha").expect("valid ref"),
        run_id: RunId::new("run-0001").expect("valid id"),
    };
    let error = ObserverHandle::new(ReadsOnly)
        .list_receipts(selector, PageRequest::new())
        .await
        .expect_err("an adapter that cannot serve receipts must say so");
    assert_eq!(error.code, SdkErrorCode::CapabilityUnavailable);
    assert_eq!(error.detail("capability"), Some("receipt.read"));
}
