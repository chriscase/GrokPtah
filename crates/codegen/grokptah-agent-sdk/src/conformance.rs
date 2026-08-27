//! Adapter-agnostic contract battery.
//!
//! ADR-002 §7 step 3 requires desktop/service parity be proven "with a shared,
//! versioned fixture matrix and stated pass criteria against both hosts". This
//! module is that matrix, expressed as executable checks rather than prose, so
//! every adapter is held to one definition of the contract instead of each one
//! being tested against its own author's assumptions.
//!
//! An adapter supplies a [`Harness`]; the battery drives it. Checks that need a
//! fault the harness cannot produce are **skipped and reported as skipped**,
//! never silently passed — a matrix that quietly counts unrunnable checks as
//! green is worse than no matrix.

use crate::capability::CapabilityId;
use crate::client::{AgentControlPlane, AgentControlPlaneExt};
use crate::dto::*;
use crate::error::SdkErrorCode;
use crate::ids::{AgentId, ArtifactId, AttemptId, RequestId, RunId, WorkId, WorkspaceRef};
use crate::page::PageRequest;

/// One check's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Passed,
    /// The harness cannot produce this precondition on this adapter.
    Skipped(String),
    Failed(String),
}

impl CheckOutcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

/// One named check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub name: &'static str,
    pub outcome: CheckOutcome,
}

/// The whole battery's result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConformanceReport {
    pub checks: Vec<CheckResult>,
}

impl ConformanceReport {
    pub fn failures(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| c.outcome.is_failure())
    }

    pub fn skipped(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks
            .iter()
            .filter(|c| matches!(c.outcome, CheckOutcome::Skipped(_)))
    }

    pub fn passed_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Passed)
            .count()
    }

    /// `true` only when nothing failed. Skips do not fail the battery, but they
    /// are visible in [`Self::summary`] so coverage gaps stay honest.
    pub fn is_clean(&self) -> bool {
        self.failures().next().is_none()
    }

    pub fn summary(&self) -> String {
        let failed: Vec<String> = self
            .failures()
            .map(|c| match &c.outcome {
                CheckOutcome::Failed(why) => format!("  FAILED {}: {why}", c.name),
                _ => unreachable!("failures() yields only failures"),
            })
            .collect();
        let skipped: Vec<String> = self
            .skipped()
            .map(|c| match &c.outcome {
                CheckOutcome::Skipped(why) => format!("  skipped {}: {why}", c.name),
                _ => unreachable!("skipped() yields only skips"),
            })
            .collect();
        let mut out = format!(
            "{} passed, {} failed, {} skipped",
            self.passed_count(),
            failed.len(),
            skipped.len()
        );
        for line in failed.into_iter().chain(skipped) {
            out.push('\n');
            out.push_str(&line);
        }
        out
    }
}

/// What an adapter must provide for the battery to drive it.
#[async_trait::async_trait]
pub trait Harness: Send + Sync {
    /// The adapter under test.
    fn plane(&self) -> &dyn AgentControlPlane;

    /// A session this credential owns, on a workspace this host allowlists.
    async fn owned_session(&self) -> SessionView;

    /// A workspace this host does **not** allowlist, if the harness can name
    /// one. Used to prove the allowlist gate precedes the scope gate.
    async fn foreign_workspace(&self) -> Option<WorkspaceRef> {
        None
    }

    /// A session belonging to another owner, if the harness can produce one.
    async fn cross_tenant_session(&self) -> Option<SessionView> {
        None
    }

    /// Arrange for the next call to fail as a dropped connection.
    async fn arm_lost_connection(&self) -> bool {
        false
    }

    /// Arrange for the next mutation to report an uncertain outcome.
    async fn arm_uncertain_send(&self) -> bool {
        false
    }

    /// Drive `run_id` to a terminal state so artifacts exist.
    async fn drive_to_completion(&self, run_id: &RunId) -> bool;

    /// Expire this run's early events so a stale cursor can be tested.
    async fn expire_early_events(&self, _run_id: &RunId) -> bool {
        false
    }

    /// Corrupt an artifact body without restamping its digest.
    async fn corrupt_artifact(&self, _run_id: &RunId, _artifact_id: &ArtifactId) -> bool {
        false
    }

    /// A work item this credential may claim, when the harness has one.
    /// Returning `None` skips the lease checks rather than passing them.
    async fn claimable_work(&self) -> Option<(WorkId, AgentId)> {
        None
    }

    /// Mint a fresh idempotency key. Must never repeat within one battery run.
    fn next_request_id(&self) -> RequestId;
}

/// The prompt every battery submission uses. Named so redaction checks can
/// assert that no projection echoes it back.
const CONFORMANCE_PROMPT: &str = "conformance: describe this project";

macro_rules! check {
    ($report:expr, $name:literal, $body:expr) => {{
        let outcome: CheckOutcome = $body;
        $report.checks.push(CheckResult {
            name: $name,
            outcome,
        });
    }};
}

fn expect_code(actual: &crate::error::SdkError, want: SdkErrorCode, what: &str) -> CheckOutcome {
    if actual.code == want {
        CheckOutcome::Passed
    } else {
        CheckOutcome::Failed(format!("{what}: expected {want}, got {}", actual.code))
    }
}

/// Run the full battery against one adapter.
pub async fn run_battery<H: Harness>(harness: &H) -> ConformanceReport {
    let mut report = ConformanceReport::default();
    let plane = harness.plane();
    let session = harness.owned_session().await;

    // ── Discovery and version negotiation ────────────────────────────────
    let connected = match plane.connect().await {
        Ok(connected) => connected,
        Err(error) => {
            report.checks.push(CheckResult {
                name: "discovery.connect",
                outcome: CheckOutcome::Failed(format!("connect failed: {error}")),
            });
            return report;
        }
    };
    check!(report, "discovery.connect", CheckOutcome::Passed);

    check!(report, "discovery.forbidden_capabilities_are_denied", {
        let mut outcome = CheckOutcome::Passed;
        for id in CapabilityId::permanently_forbidden() {
            match connected.document.require(id) {
                Ok(_) => {
                    outcome = CheckOutcome::Failed(format!("{id} must never be available"));
                    break;
                }
                Err(error) if error.code == SdkErrorCode::ForbiddenScope => {}
                Err(error) => {
                    outcome = CheckOutcome::Failed(format!(
                        "{id} denied with {} instead of forbidden_scope",
                        error.code
                    ));
                    break;
                }
            }
        }
        outcome
    });

    check!(
        report,
        "discovery.unadvertised_is_capability_unavailable",
        {
            // A capability nobody advertises must be distinguishable from one that
            // is advertised and denied.
            match connected
                .document
                .require(&CapabilityId::Unknown("never.advertised".into()))
            {
                Ok(_) => CheckOutcome::Failed("unadvertised capability resolved".into()),
                Err(error) => expect_code(
                    &error,
                    SdkErrorCode::CapabilityUnavailable,
                    "unadvertised capability",
                ),
            }
        }
    );

    // ── Submit / observe / events ────────────────────────────────────────
    if connected.require(&CapabilityId::TaskSubmit).is_err() {
        report.checks.push(CheckResult {
            name: "submit.success",
            outcome: CheckOutcome::Skipped("task.submit is not available on this host".into()),
        });
        return report;
    }

    let submission = TaskSubmission {
        request_id: harness.next_request_id(),
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        prompt: CONFORMANCE_PROMPT.into(),
        bounds: None,
        execution_mode: ExecutionMode::Shared,
        allow_queue: false,
    };
    let accepted = match plane.submit_task(submission.clone()).await {
        Ok(accepted) => {
            report.checks.push(CheckResult {
                name: "submit.success",
                outcome: if accepted.replayed == Some(true) {
                    CheckOutcome::Failed("a first submission must not report replayed".into())
                } else {
                    CheckOutcome::Passed
                },
            });
            accepted
        }
        Err(error) => {
            report.checks.push(CheckResult {
                name: "submit.success",
                outcome: CheckOutcome::Failed(format!("submit failed: {error}")),
            });
            return report;
        }
    };

    check!(report, "submit.replay_is_idempotent", {
        // The invariant that matters is that the same key never does the
        // work twice. Whether the host can *say* it replayed is secondary:
        // the MCP boundary replays a stored receipt byte-for-byte, so an
        // adapter there cannot tell. `Some(false)` is still a failure — that
        // is a host claiming it did fresh work under a used key.
        match plane.submit_task(submission.clone()).await {
            Ok(replay) if replay.run_id != accepted.run_id => {
                CheckOutcome::Failed("replay created a second run".into())
            }
            Ok(replay) if replay.replayed == Some(false) => {
                CheckOutcome::Failed("replay reported itself as fresh work".into())
            }
            Ok(_) => CheckOutcome::Passed,
            Err(error) => CheckOutcome::Failed(format!("replay failed: {error}")),
        }
    });

    check!(report, "submit.same_key_new_payload_conflicts", {
        let mutated = TaskSubmission {
            prompt: "conformance: a different instruction".into(),
            ..submission.clone()
        };
        match plane.submit_task(mutated).await {
            Ok(_) => CheckOutcome::Failed("reused key with a new payload was accepted".into()),
            Err(error) => expect_code(&error, SdkErrorCode::Conflict, "reused idempotency key"),
        }
    });

    let selector = RunSelector {
        session_id: session.session_id.clone(),
        workspace: session.workspace.clone(),
        run_id: accepted.run_id.clone(),
    };

    check!(report, "observe.projection_is_readable", {
        match plane.observe_run(selector.clone()).await {
            Ok(view) if view.run_id == accepted.run_id => CheckOutcome::Passed,
            Ok(_) => CheckOutcome::Failed("observe returned another run".into()),
            Err(error) => CheckOutcome::Failed(format!("observe failed: {error}")),
        }
    });

    check!(report, "observe.revision_is_monotonic", {
        let mut watermark = RevisionWatermark::new();
        match plane.observe_run(selector.clone()).await {
            Err(error) => CheckOutcome::Failed(format!("observe failed: {error}")),
            Ok(view) => {
                if watermark.admit(view.revision).is_err() {
                    CheckOutcome::Failed("first observation did not advance the watermark".into())
                } else if watermark.admit(view.revision).is_ok() {
                    CheckOutcome::Failed("a repeated revision was admitted".into())
                } else {
                    CheckOutcome::Passed
                }
            }
        }
    });

    // ── Authorization and scope ──────────────────────────────────────────
    check!(report, "authz.cross_session_read_is_forbidden_scope", {
        match RunId::new("run-does-not-exist") {
            Err(error) => CheckOutcome::Failed(format!("could not mint a probe id: {error}")),
            Ok(run_id) => {
                let forged = RunSelector {
                    run_id,
                    ..selector.clone()
                };
                match plane.observe_run(forged).await {
                    Ok(_) => CheckOutcome::Failed("an unknown run was readable".into()),
                    Err(error) => expect_code(&error, SdkErrorCode::ForbiddenScope, "unknown run"),
                }
            }
        }
    });

    check!(report, "authz.foreign_workspace_is_workspace_mismatch", {
        match harness.foreign_workspace().await {
            None => CheckOutcome::Skipped("harness cannot name a non-allowlisted workspace".into()),
            Some(workspace) => {
                let forged = RunSelector {
                    workspace,
                    ..selector.clone()
                };
                match plane.observe_run(forged).await {
                    Ok(_) => {
                        CheckOutcome::Failed("a non-allowlisted workspace was readable".into())
                    }
                    Err(error) => expect_code(
                        &error,
                        SdkErrorCode::WorkspaceMismatch,
                        "non-allowlisted workspace",
                    ),
                }
            }
        }
    });

    check!(report, "authz.cross_tenant_read_is_indistinguishable", {
        match harness.cross_tenant_session().await {
            None => CheckOutcome::Skipped("harness cannot produce a cross-tenant session".into()),
            Some(other) => {
                let forged = RunSelector {
                    session_id: other.session_id,
                    workspace: other.workspace,
                    run_id: accepted.run_id.clone(),
                };
                match plane.observe_run(forged).await {
                    Ok(_) => CheckOutcome::Failed("a cross-tenant run was readable".into()),
                    Err(error) => {
                        expect_code(&error, SdkErrorCode::ForbiddenScope, "cross-tenant read")
                    }
                }
            }
        }
    });

    // ── Transport faults ─────────────────────────────────────────────────
    check!(report, "faults.lost_connection_is_safely_retryable", {
        if !harness.arm_lost_connection().await {
            CheckOutcome::Skipped("harness cannot arm a lost connection".into())
        } else {
            match plane.observe_run(selector.clone()).await {
                Ok(_) => CheckOutcome::Failed("armed lost connection did not surface".into()),
                Err(error) if error.code == SdkErrorCode::TransportUnavailable => {
                    if error.code.is_safely_retryable() {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed(
                            "transport_unavailable must be safely retryable".into(),
                        )
                    }
                }
                Err(error) => expect_code(
                    &error,
                    SdkErrorCode::TransportUnavailable,
                    "armed lost connection",
                ),
            }
        }
    });

    check!(report, "faults.uncertain_send_is_never_auto_retried", {
        if !harness.arm_uncertain_send().await {
            CheckOutcome::Skipped("harness cannot arm an uncertain send".into())
        } else {
            let request = FollowUpRequest {
                request_id: harness.next_request_id(),
                session_id: session.session_id.clone(),
                workspace: session.workspace.clone(),
                text: "conformance: uncertain".into(),
                expected_revision: None,
            };
            match plane.request_follow_up(request).await {
                Ok(_) => CheckOutcome::Failed("armed uncertain send did not surface".into()),
                Err(error) if error.code == SdkErrorCode::UncertainOutcome => {
                    if error.retry_disposition() == crate::error::RetryDisposition::Unsafe {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed(
                            "uncertain_outcome must classify as unsafe to retry".into(),
                        )
                    }
                }
                Err(error) => expect_code(
                    &error,
                    SdkErrorCode::UncertainOutcome,
                    "armed uncertain send",
                ),
            }
        }
    });

    // ── Follow-up ────────────────────────────────────────────────────────
    check!(report, "followup.accepted_without_cancelling", {
        let request = FollowUpRequest {
            request_id: harness.next_request_id(),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "conformance: also check the README".into(),
            expected_revision: None,
        };
        match plane.request_follow_up(request).await {
            Err(error) => CheckOutcome::Failed(format!("follow-up failed: {error}")),
            Ok(receipt) => match plane.observe_run(selector.clone()).await {
                Err(error) => CheckOutcome::Failed(format!("observe failed: {error}")),
                Ok(view) if view.lifecycle == RunLifecycle::Cancelled => {
                    CheckOutcome::Failed("follow-up cancelled the run".into())
                }
                Ok(_) => {
                    if receipt.revision.value() > 0 {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed("follow-up receipt carried no revision".into())
                    }
                }
            },
        }
    });

    check!(report, "followup.stale_fence_is_rejected_without_effect", {
        let request = FollowUpRequest {
            request_id: harness.next_request_id(),
            session_id: session.session_id.clone(),
            workspace: session.workspace.clone(),
            text: "conformance: stale fence".into(),
            expected_revision: Some(Revision::new(0)),
        };
        match plane.request_follow_up(request).await {
            Ok(_) => CheckOutcome::Failed("a stale revision fence was accepted".into()),
            Err(error) if error.code == SdkErrorCode::StaleVersion => CheckOutcome::Passed,
            // Refusing the fence outright is correct for a host with no
            // compare-and-set on this operation. Silently dropping it would
            // not be, and that is what the `Ok(_)` arm above catches.
            Err(error) if error.code == SdkErrorCode::Unsupported => {
                CheckOutcome::Skipped("host does not fence follow-up on a revision".into())
            }
            Err(error) => expect_code(&error, SdkErrorCode::StaleVersion, "stale fence"),
        }
    });

    // ── Terminal state, events, artifacts ────────────────────────────────
    if !harness.drive_to_completion(&accepted.run_id).await {
        report.checks.push(CheckResult {
            name: "events.page_and_resume",
            outcome: CheckOutcome::Skipped("harness cannot drive a run to completion".into()),
        });
        return report;
    }

    check!(report, "events.page_and_resume", {
        match plane
            .stream_events(selector.clone(), PageRequest::new().limit(1))
            .await
        {
            Err(error) => CheckOutcome::Failed(format!("first page failed: {error}")),
            Ok(first) if first.items.is_empty() => {
                CheckOutcome::Failed("a completed run produced no events".into())
            }
            Ok(first) => match first.next_cursor.clone() {
                None => CheckOutcome::Failed("a bounded page reported no continuation".into()),
                Some(cursor) => match plane
                    .stream_events(selector.clone(), PageRequest::new().after(cursor).limit(1))
                    .await
                {
                    Err(error) => CheckOutcome::Failed(format!("resume failed: {error}")),
                    Ok(second) if second.items == first.items => {
                        CheckOutcome::Failed("resuming re-delivered the same event".into())
                    }
                    Ok(_) => CheckOutcome::Passed,
                },
            },
        }
    });

    check!(report, "events.oversized_limit_is_rejected", {
        match plane
            .stream_events(
                selector.clone(),
                PageRequest::new().limit(crate::page::MAX_PAGE_LIMIT + 1),
            )
            .await
        {
            Ok(_) => CheckOutcome::Failed("an over-ceiling page limit was accepted".into()),
            Err(error) => expect_code(&error, SdkErrorCode::InvalidRequest, "oversized limit"),
        }
    });

    check!(report, "events.expired_cursor_reports_retained_range", {
        if !harness.expire_early_events(&accepted.run_id).await {
            CheckOutcome::Skipped("harness cannot expire retained events".into())
        } else {
            let stale = crate::page::Cursor::from_opaque("1");
            match plane
                .stream_events(selector.clone(), PageRequest::new().after(stale))
                .await
            {
                Ok(_) => CheckOutcome::Failed("an expired cursor was served".into()),
                Err(error) if error.code == SdkErrorCode::CursorExpired => {
                    if error.detail("retainedStart").is_some()
                        && error.detail("retainedEnd").is_some()
                    {
                        CheckOutcome::Passed
                    } else {
                        CheckOutcome::Failed(
                            "cursor_expired did not carry the retained range".into(),
                        )
                    }
                }
                Err(error) => expect_code(&error, SdkErrorCode::CursorExpired, "expired cursor"),
            }
        }
    });

    let artifact_id = match plane.observe_run(selector.clone()).await {
        Ok(view) => view.artifacts.first().map(|a| a.artifact_id.clone()),
        Err(_) => None,
    };

    check!(report, "artifacts.fetch_is_verified", {
        match &artifact_id {
            None => CheckOutcome::Skipped("run produced no artifacts".into()),
            Some(id) => {
                let request = ArtifactRequest {
                    selector: selector.clone(),
                    artifact_id: id.clone(),
                    max_bytes: None,
                };
                match plane.fetch_artifact(request).await {
                    Err(error) => CheckOutcome::Failed(format!("fetch failed: {error}")),
                    Ok(payload) => match payload.verify(crate::dto::MAX_ARTIFACT_BYTES as u64) {
                        Ok(()) => CheckOutcome::Passed,
                        Err(error) => CheckOutcome::Failed(format!(
                            "adapter returned an artifact that fails verification: {error}"
                        )),
                    },
                }
            }
        }
    });

    check!(report, "artifacts.over_ceiling_request_is_rejected", {
        match &artifact_id {
            None => CheckOutcome::Skipped("run produced no artifacts".into()),
            Some(id) => {
                let request = ArtifactRequest {
                    selector: selector.clone(),
                    artifact_id: id.clone(),
                    max_bytes: Some(1),
                };
                match plane.fetch_artifact(request).await {
                    Ok(_) => CheckOutcome::Failed("a 1-byte ceiling returned a body".into()),
                    Err(error) => {
                        expect_code(&error, SdkErrorCode::InvalidRequest, "artifact ceiling")
                    }
                }
            }
        }
    });

    check!(report, "artifacts.digest_mismatch_is_integrity_error", {
        match &artifact_id {
            None => CheckOutcome::Skipped("run produced no artifacts".into()),
            Some(id) => {
                if !harness.corrupt_artifact(&accepted.run_id, id).await {
                    CheckOutcome::Skipped("harness cannot corrupt an artifact".into())
                } else {
                    let request = ArtifactRequest {
                        selector: selector.clone(),
                        artifact_id: id.clone(),
                        max_bytes: None,
                    };
                    match plane.fetch_artifact(request).await {
                        Ok(_) => CheckOutcome::Failed("a corrupted artifact was returned".into()),
                        Err(error) => expect_code(
                            &error,
                            SdkErrorCode::IntegrityMismatch,
                            "corrupted artifact",
                        ),
                    }
                }
            }
        }
    });

    // ── Cancellation ─────────────────────────────────────────────────────
    check!(report, "cancel.is_idempotent", {
        let request = CancelRequest {
            request_id: harness.next_request_id(),
            selector: selector.clone(),
        };
        match plane.cancel_run(request.clone()).await {
            Err(error) => CheckOutcome::Failed(format!("cancel failed: {error}")),
            Ok(first) => match plane.cancel_run(request).await {
                Err(error) => CheckOutcome::Failed(format!("cancel replay failed: {error}")),
                Ok(second) if second.lifecycle != first.lifecycle => {
                    CheckOutcome::Failed("cancel replay changed the lifecycle".into())
                }
                Ok(second) if second.replayed == Some(false) => {
                    CheckOutcome::Failed("cancel replay reported itself as fresh work".into())
                }
                Ok(_) => CheckOutcome::Passed,
            },
        }
    });

    // ── Redacted receipts ────────────────────────────────────────────────
    check!(report, "receipts.are_scoped_and_do_not_echo_the_request", {
        match plane
            .list_receipts(selector.clone(), PageRequest::new())
            .await
        {
            Err(error)
                if matches!(
                    error.code,
                    SdkErrorCode::CapabilityUnavailable | SdkErrorCode::Unsupported
                ) =>
            {
                CheckOutcome::Skipped("adapter does not serve redacted receipts".into())
            }
            Err(error) => CheckOutcome::Failed(format!("receipts failed: {error}")),
            Ok(page) => {
                let encoded = serde_json::to_string(&page).unwrap_or_default();
                if encoded.contains(CONFORMANCE_PROMPT) {
                    CheckOutcome::Failed(
                        "a receipt echoed the prompt of the request that produced it".into(),
                    )
                } else if page.items.iter().any(|receipt| {
                    receipt
                        .run_id
                        .as_ref()
                        .is_some_and(|id| id != &selector.run_id)
                }) {
                    CheckOutcome::Failed("a receipt from another run was listed".into())
                } else {
                    CheckOutcome::Passed
                }
            }
        }
    });

    // ── Control lease ────────────────────────────────────────────────────
    check!(report, "lease.round_trip_holds_its_credential", {
        match harness.claimable_work().await {
            None => CheckOutcome::Skipped("harness has no claimable work item".into()),
            Some((work_id, claimant)) => {
                let acquire = ControlLeaseRequest {
                    request_id: harness.next_request_id(),
                    session_id: session.session_id.clone(),
                    workspace: session.workspace.clone(),
                    work_id: work_id.clone(),
                    claimant,
                    requested_ttl_ms: Some(30_000),
                };
                match plane.acquire_control(acquire).await {
                    Err(error) => CheckOutcome::Failed(format!("claim failed: {error}")),
                    Ok(lease) => {
                        let encoded = serde_json::to_string(&lease).unwrap_or_default();
                        if !lease.credential.is_empty()
                            && encoded.contains(lease.credential.reveal())
                        {
                            CheckOutcome::Failed(
                                "the lease credential reached the serialized lease".into(),
                            )
                        } else {
                            let release = ReleaseLeaseRequest {
                                request_id: harness.next_request_id(),
                                session_id: session.session_id.clone(),
                                workspace: session.workspace.clone(),
                                work_id,
                                attempt_id: lease.attempt_id.clone(),
                                reason: BoundedText::new("conformance"),
                                credential: lease.credential.clone(),
                            };
                            match plane.release_control(release).await {
                                Ok(_) => CheckOutcome::Passed,
                                Err(error) => {
                                    CheckOutcome::Failed(format!("release failed: {error}"))
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    check!(report, "lease.release_without_a_credential_fails_closed", {
        match harness.claimable_work().await {
            None => CheckOutcome::Skipped("harness has no claimable work item".into()),
            Some((work_id, _)) => match AttemptId::new("attempt-probe") {
                Err(error) => CheckOutcome::Failed(format!("could not mint a probe id: {error}")),
                Ok(attempt_id) => {
                    let release = ReleaseLeaseRequest {
                        request_id: harness.next_request_id(),
                        session_id: session.session_id.clone(),
                        workspace: session.workspace.clone(),
                        work_id,
                        attempt_id,
                        reason: BoundedText::new("conformance"),
                        credential: LeaseCredential::default(),
                    };
                    match plane.release_control(release).await {
                        Ok(_) => {
                            CheckOutcome::Failed("a credential-less release was accepted".into())
                        }
                        Err(_) => CheckOutcome::Passed,
                    }
                }
            },
        }
    });

    report
}
