//! The four headless port operations and the discipline they share.
//!
//! Every operation runs the same preamble, in this order:
//!
//! 1. structural validation of the request;
//! 2. **renegotiation** — the host's identity, capability revision, and limits
//!    are read fresh, never reused from bind time;
//! 3. binding freshness — a moved host, protocol version, or capability
//!    revision fails closed as `stale_binding`;
//! 4. tier ∩ declared capability — an operation the principal's tier forbids
//!    is `forbidden_scope`; one the host does not declare is `unsupported`;
//! 5. limit resolution against the **freshly negotiated** limits.
//!
//! A mutation then adds:
//!
//! 6. delivery classification from durable evidence — an existing claim is
//!    reported, never replayed;
//! 7. an **authorization recheck at the effect boundary**, immediately before
//!    the effect, whose one-use result the effect call consumes.
//!
//! Reads are principal-scoped: the binding is the authorization identity, and
//! unknown, cross-session, cross-workspace, and malformed resources collapse
//! into one identical `forbidden_scope` failure so a read cannot be used as an
//! existence oracle.

use chrono::{DateTime, Utc};

use super::authority::{EffectAuthorization, HeadlessAuthority, PortBinding, PortEventFacts};
use super::projection::{
    project_review, project_run_at, PortCancelView, PortEvent, PortEventPage, PortEventsView,
    PortReviewProjection, PortSubmitView,
};
use super::types::{
    validate_identifier, HostNegotiation, PortCancelReceipt, PortClaimState, PortDelivery,
    PortDeliveryEvidence, PortError, PortErrorCode, PortOperation, PortPrincipal, PortResult,
    PortRunFacts, PortSubmitReceipt, PortSubmitRequest,
};

/// Host-neutral headless agent port.
///
/// The port owns protocol discipline only. Sending, persistence, redaction of
/// stored records, and recovery all stay in the runtime the authority adapts;
/// nothing here re-implements them.
pub struct HeadlessAgentPort<A: HeadlessAuthority> {
    authority: A,
}

impl<A: HeadlessAuthority> HeadlessAgentPort<A> {
    pub fn new(authority: A) -> Self {
        Self { authority }
    }

    pub fn authority(&self) -> &A {
        &self.authority
    }

    /// Read the host's declared identity, capabilities, and limits. An
    /// embedder calls this once to mint a binding, and the port calls it again
    /// before every operation.
    pub async fn negotiate(&self, principal: &PortPrincipal) -> PortResult<HostNegotiation> {
        let negotiation = self.authority.negotiate(principal).await?;
        if negotiation.protocol_version != super::types::HEADLESS_PORT_PROTOCOL_VERSION {
            return Err(PortError::new(
                PortErrorCode::StaleBinding,
                "host negotiated a different headless port protocol version",
            ));
        }
        negotiation.limits.validate()?;
        Ok(negotiation)
    }

    /// Submit one bounded task.
    ///
    /// The returned view is the typed answer for *every* durable delivery
    /// state, including the ones that mean "do not try this request id again":
    /// `sending`, `uncertain`, and `rejected` come back as receipts, not as
    /// errors, so an embedder cannot lose them by only inspecting the error
    /// path. An `Err` here means the request never became a durable claim.
    pub async fn submit(
        &self,
        binding: &PortBinding,
        request: &PortSubmitRequest,
        now: DateTime<Utc>,
    ) -> PortResult<PortSubmitView> {
        let request_id = validate_identifier(request.request_id.clone())?;
        let negotiation = self.preflight(binding, PortOperation::Submit).await?;
        let limits = request.bounds.resolve(&negotiation.limits)?;
        if request.prompt.len() > limits.max_prompt_bytes {
            return Err(PortError::new(
                PortErrorCode::LimitExceeded,
                "prompt exceeds the negotiated max prompt bytes",
            ));
        }

        // Write-ahead evidence decides whether this request id may act. A
        // prior claim is reported as-is; the port never re-sends for it.
        let evidence = self
            .authority
            .delivery_evidence(binding, &request_id)
            .await?;
        require_claim_operation(&evidence, PortOperation::Submit)?;
        let delivery = classify_delivery(&evidence, &negotiation);
        if delivery != PortDelivery::Unknown {
            return Ok(settled_submit_view(&request_id, delivery, &evidence, now));
        }

        let authorization = self.authorize(binding, PortOperation::Submit).await?;
        let facts = self
            .authority
            .perform_submit(
                binding,
                authorization,
                &request_id,
                &request.prompt,
                &limits,
                request.execution_mode,
                request.allow_queue,
            )
            .await?;
        let facts = self.require_attributed(&facts, &request_id, binding)?;
        Ok(PortSubmitView {
            receipt: PortSubmitReceipt {
                request_id,
                delivery: PortDelivery::Delivered,
                run_id: Some(facts.run_id.clone()),
                queued_position: facts.queued_position,
                retry_with_same_request_id: false,
                rejection: None,
                admitted_limits: Some(limits),
            },
            run: Some(project_run_at(&facts, PortDelivery::Delivered, now)),
        })
    }

    /// Read one bounded page of a run's durable journal plus the authoritative
    /// run projection at the same instant.
    pub async fn events(
        &self,
        binding: &PortBinding,
        run_id: &str,
        after_seq: u64,
        limit: usize,
        now: DateTime<Utc>,
    ) -> PortResult<PortEventsView> {
        let run_id =
            validate_identifier(run_id.to_string()).map_err(|_| super::types::scope_denied())?;
        let negotiation = self.preflight(binding, PortOperation::Events).await?;
        let applied_limit = negotiation.limits.clamp_page(limit);
        let facts = self.authority.run_facts(binding, &run_id).await?;
        let facts = self.require_attributed_run(&facts, binding, &run_id)?;
        let page_facts = self
            .authority
            .run_events(binding, &run_id, after_seq, applied_limit)
            .await?;
        let page = bounded_page(&run_id, &facts, &page_facts, after_seq, applied_limit)?;
        Ok(PortEventsView {
            run: project_run_at(&facts, PortDelivery::Delivered, now),
            page,
        })
    }

    /// Read the review decision surface for a reviewable run: promotion state,
    /// fingerprints, and counts. Diff bytes and changed paths never cross the
    /// port.
    pub async fn review(
        &self,
        binding: &PortBinding,
        run_id: &str,
    ) -> PortResult<PortReviewProjection> {
        let run_id =
            validate_identifier(run_id.to_string()).map_err(|_| super::types::scope_denied())?;
        self.preflight(binding, PortOperation::Review).await?;
        let facts = self.authority.run_facts(binding, &run_id).await?;
        let facts = self.require_attributed_run(&facts, binding, &run_id)?;
        let review = self.authority.review_facts(binding, &run_id).await?;
        if review.run_id != facts.run_id {
            return Err(super::types::scope_denied());
        }
        Ok(project_review(&facts, &review))
    }

    /// Cancel one bounded run. Cancellation is a durable effect and takes the
    /// same write path as submit, including the effect-boundary recheck.
    pub async fn cancel(
        &self,
        binding: &PortBinding,
        request_id: &str,
        run_id: &str,
        now: DateTime<Utc>,
    ) -> PortResult<PortCancelView> {
        let request_id = validate_identifier(request_id.to_string())?;
        let run_id =
            validate_identifier(run_id.to_string()).map_err(|_| super::types::scope_denied())?;
        let negotiation = self.preflight(binding, PortOperation::Cancel).await?;

        let evidence = self
            .authority
            .delivery_evidence(binding, &request_id)
            .await?;
        require_claim_operation(&evidence, PortOperation::Cancel)?;
        // A claim that names another run is the same request id pointed at a
        // different effect. Replaying its receipt would answer a question the
        // caller did not ask.
        if let Some(claimed) = evidence
            .claim
            .as_ref()
            .and_then(|claim| claim.run_id.as_deref())
        {
            if claimed != run_id {
                return Err(PortError::new(
                    PortErrorCode::Conflict,
                    "request id was already used to cancel a different run",
                ));
            }
        }
        let delivery = classify_delivery(&evidence, &negotiation);
        if delivery != PortDelivery::Unknown {
            let run = evidence
                .run
                .as_ref()
                .map(|facts| project_run_at(facts, delivery, now));
            return Ok(PortCancelView {
                receipt: PortCancelReceipt {
                    request_id,
                    run_id,
                    delivery,
                    retry_with_same_request_id: delivery.retry_with_same_request_id(),
                    rejection: evidence.claim.and_then(|claim| claim.rejection),
                },
                run,
            });
        }

        // Scope the run before causing any effect, so an out-of-scope run is
        // refused by the read gate rather than by the mutation itself.
        let scoped = self.authority.run_facts(binding, &run_id).await?;
        self.require_attributed_run(&scoped, binding, &run_id)?;
        let authorization = self.authorize(binding, PortOperation::Cancel).await?;
        let facts = self
            .authority
            .perform_cancel(binding, authorization, &request_id, &run_id)
            .await?;
        let facts = self.require_attributed_run(&facts, binding, &run_id)?;
        Ok(PortCancelView {
            receipt: PortCancelReceipt {
                request_id,
                run_id,
                delivery: PortDelivery::Delivered,
                retry_with_same_request_id: false,
                rejection: None,
            },
            run: Some(project_run_at(&facts, PortDelivery::Delivered, now)),
        })
    }

    /// Steps 2–5 of the shared preamble.
    async fn preflight(
        &self,
        binding: &PortBinding,
        operation: PortOperation,
    ) -> PortResult<HostNegotiation> {
        let negotiation = self.negotiate(binding.principal()).await?;
        binding.require_current(&negotiation)?;
        if !binding.principal().tier.permits(operation) {
            return Err(PortError::new(
                PortErrorCode::ForbiddenScope,
                "principal tier does not permit this operation",
            ));
        }
        if !negotiation.declares(operation) {
            return Err(PortError::new(
                PortErrorCode::Unsupported,
                "host does not declare this operation at the negotiated capability revision",
            ));
        }
        Ok(negotiation)
    }

    /// Step 7: recheck authority at the effect boundary and validate that the
    /// authorization the host issued describes this exact effect.
    async fn authorize(
        &self,
        binding: &PortBinding,
        operation: PortOperation,
    ) -> PortResult<EffectAuthorization> {
        let authorization = self.authority.authorize_effect(binding, operation).await?;
        if !authorization.matches(binding, operation) {
            return Err(PortError::new(
                PortErrorCode::ForbiddenScope,
                "effect authorization does not match the bound principal, scope, or operation",
            ));
        }
        Ok(authorization)
    }

    /// A host must not hand back a run belonging to another session or to a
    /// different request id than the one just performed.
    fn require_attributed(
        &self,
        facts: &PortRunFacts,
        request_id: &str,
        binding: &PortBinding,
    ) -> PortResult<PortRunFacts> {
        if facts.request_id != request_id || facts.session_id != binding.session_id() {
            return Err(super::types::scope_denied());
        }
        Ok(facts.clone())
    }

    fn require_attributed_run(
        &self,
        facts: &PortRunFacts,
        binding: &PortBinding,
        run_id: &str,
    ) -> PortResult<PortRunFacts> {
        if facts.session_id != binding.session_id() || facts.run_id != run_id {
            return Err(super::types::scope_denied());
        }
        Ok(facts.clone())
    }
}

/// A request id belongs to one operation. Reusing it for another is a
/// conflict rather than a replay, because the recorded receipt answers a
/// different question than the one being asked.
fn require_claim_operation(
    evidence: &PortDeliveryEvidence,
    operation: PortOperation,
) -> PortResult<()> {
    let recorded = evidence
        .claim
        .as_ref()
        .and_then(|claim| claim.operation)
        .unwrap_or(operation);
    if recorded != operation {
        return Err(PortError::new(
            PortErrorCode::Conflict,
            "request id was already used for a different operation",
        ));
    }
    Ok(())
}

/// Classify one request id's durable delivery state.
///
/// The rule that matters: a claim that did not settle cleanly is `uncertain`
/// whenever its effect could still have landed, and `uncertain` is never
/// retryable under the same request id. Nothing here performs or replays an
/// effect — it reads durable evidence only.
pub(crate) fn classify_delivery(
    evidence: &PortDeliveryEvidence,
    negotiation: &HostNegotiation,
) -> PortDelivery {
    let Some(claim) = evidence.claim.as_ref() else {
        // A run attributed to this request id with no durable claim means the
        // effect happened and the acknowledgement did not.
        return if evidence.run.is_some() {
            PortDelivery::Uncertain
        } else {
            PortDelivery::Unknown
        };
    };
    match claim.state {
        PortClaimState::Completed => PortDelivery::Delivered,
        PortClaimState::Claimed => {
            if claim.claimed_at < negotiation.generation_started_at {
                // Written ahead by a generation that is gone. Whether the
                // effect landed cannot be known from here.
                PortDelivery::Uncertain
            } else {
                PortDelivery::Sending
            }
        }
        PortClaimState::FailedInterrupted => PortDelivery::Uncertain,
        PortClaimState::FailedRejected => {
            if evidence.run.is_some() {
                // A refusal that nonetheless produced a run is not a refusal.
                PortDelivery::Uncertain
            } else {
                PortDelivery::Rejected
            }
        }
    }
}

fn settled_submit_view(
    request_id: &str,
    delivery: PortDelivery,
    evidence: &PortDeliveryEvidence,
    now: DateTime<Utc>,
) -> PortSubmitView {
    let claim = evidence.claim.as_ref();
    let run_id = evidence
        .run
        .as_ref()
        .map(|facts| facts.run_id.clone())
        .or_else(|| claim.and_then(|claim| claim.run_id.clone()));
    PortSubmitView {
        receipt: PortSubmitReceipt {
            request_id: request_id.to_string(),
            delivery,
            run_id,
            queued_position: claim.and_then(|claim| claim.queued_position),
            retry_with_same_request_id: delivery.retry_with_same_request_id(),
            rejection: claim.and_then(|claim| claim.rejection.clone()),
            admitted_limits: evidence.run.as_ref().map(|facts| facts.admitted_limits),
        },
        run: evidence
            .run
            .as_ref()
            .map(|facts| project_run_at(facts, delivery, now)),
    }
}

/// Enforce the page contract: bounded size, strictly increasing sequences
/// above the requested cursor, a resume cursor that actually resumes, and an
/// expired cursor reported as an empty gap rather than a short stream.
fn bounded_page(
    run_id: &str,
    facts: &PortRunFacts,
    page: &PortEventFacts,
    after_seq: u64,
    applied_limit: usize,
) -> PortResult<PortEventPage> {
    let range = super::projection::event_range(facts);
    if page.cursor_expired {
        return Ok(PortEventPage {
            run_id: run_id.to_string(),
            entries: Vec::new(),
            next_cursor: None,
            cursor_expired: true,
            applied_limit,
            range,
        });
    }
    let malformed = PortError::new(
        PortErrorCode::Internal,
        "host returned a non-monotonic or out-of-range event page",
    );
    let mut entries: Vec<PortEvent> = Vec::with_capacity(page.entries.len().min(applied_limit));
    let mut previous = after_seq;
    for (seq, kind) in page.entries.iter().copied() {
        if seq <= previous {
            return Err(malformed);
        }
        if let Some(start_seq) = facts.start_seq {
            if seq < start_seq {
                return Err(malformed);
            }
        }
        if let Some(end_seq) = facts.end_seq {
            if seq > end_seq {
                return Err(malformed);
            }
        }
        previous = seq;
        entries.push(PortEvent { seq, kind });
        if entries.len() == applied_limit {
            break;
        }
    }
    let truncated = page.entries.len() > entries.len();
    let last_seq = entries.last().map(|entry| entry.seq);
    let next_cursor = match (page.next_cursor, truncated, last_seq) {
        // A host-declared resume point must be the last sequence this page
        // actually returned, otherwise resuming from it would skip entries.
        (Some(cursor), _, Some(last)) if cursor == last => Some(cursor),
        (Some(_), _, _) => return Err(malformed),
        (None, true, Some(last)) => Some(last),
        (None, _, _) => None,
    };
    Ok(PortEventPage {
        run_id: run_id.to_string(),
        entries,
        next_cursor,
        cursor_expired: false,
        applied_limit,
        range,
    })
}

/// Convenience for adapters: the instant a port operation should use when the
/// caller has no reason to supply one.
pub fn port_now() -> DateTime<Utc> {
    Utc::now()
}
