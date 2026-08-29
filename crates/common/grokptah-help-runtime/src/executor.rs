//! The one bounded executor.
//!
//! # Why the provider seam is pollable
//!
//! A blocking `send()` cannot express the distinction this executor exists to
//! preserve. When a caller cancels, two different things may be true: the
//! provider stopped, or the provider is simply not answering. A blocking call
//! collapses both into "we returned", which is how a run that is still burning
//! a remote resource gets reported as `Cancelled`.
//!
//! So [`Provider`] is begin/poll/cancel. `cancel` *requests* a stop;
//! quiescence is only ever learned from a subsequent [`Poll::Quiesced`]. Until
//! that arrives the run stays in [`RunState::Draining`] and keeps its capacity
//! slot. A provider that never answers therefore leaves a run
//! [`RunState::Abandoned`] holding a slot — the executor is genuinely one
//! attempt short, and says so, rather than freeing a slot it does not have.
//!
//! # Exactly one request
//!
//! [`Provider::begin`] is called at most once per run, and
//! [`Executor::provider_calls`] counts them so a test can prove it. There is no
//! retry, no second route, and no fallback: a failed attempt is a failed
//! attempt. Retrying inside the executor would mean a caller who cancelled
//! after the first send could still be charged for a second one.
//!
//! # Truthful send certainty
//!
//! [`Begin::Accepted`] is the only path to [`SendCertainty::Sent`]. A provider
//! that started an attempt without confirming delivery yields
//! [`SendCertainty::Unknown`], and `Unknown` is never rewritten to `NotSent` on
//! cancellation — the caller cancelling does not un-send what may have left.

use std::collections::VecDeque;

use grokptah_help_authority::{Authority, Checkpoint};
use grokptah_help_contract::dto::{
    Admission, BoundsProjection, DenyReason, Grant, HelpRequest, Outcome, PublicErrorCode, Receipt,
    RedactionCount, SendCertainty,
};

/// Fixed limits. Not configuration a caller can widen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub max_concurrency: usize,
    pub max_queued: usize,
    pub deadline_ms: u64,
    /// How long after a cancel or deadline the host waits for quiescence
    /// before recording the run as abandoned. The slot is still not released.
    pub abandon_after_ms: u64,
}

impl Default for Bounds {
    fn default() -> Self {
        Self {
            max_concurrency: 2,
            max_queued: 8,
            deadline_ms: 30_000,
            abandon_after_ms: 10_000,
        }
    }
}

impl Bounds {
    /// Render the bounds for a surface. The four capability flags are
    /// constants: there is no code path here that reads a tool, a history, a
    /// workspace, or a fallback route.
    #[must_use]
    pub const fn projection(&self) -> BoundsProjection {
        BoundsProjection {
            max_concurrency: self.max_concurrency,
            max_queued: self.max_queued,
            deadline_ms: self.deadline_ms,
            single_request: true,
            tools_enabled: false,
            history_enabled: false,
            workspace_enabled: false,
            fallback_enabled: false,
        }
    }
}

/// A provider ticket. Opaque to the executor.
pub type Ticket = u64;

/// What happened when the host tried to start one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begin {
    /// The provider acknowledged receipt. The host saw it leave.
    Accepted(Ticket),
    /// The attempt started and delivery is not known. This is not a failure
    /// and must not be reported as one.
    Uncertain(Ticket),
    /// Nothing left the process.
    Rejected,
}

/// The state of one in-flight attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Still working. Says nothing about whether it ever will finish.
    Pending,
    /// A raw, untrusted reply. Not yet an answer.
    Replied(String),
    /// The attempt failed and the provider has released it.
    Failed,
    /// The provider has stopped and released its resources. Only this
    /// releases a capacity slot.
    Quiesced,
}

/// The host's one-request seam to a provider.
///
/// Implementations must call nothing else on the host's behalf: no tools, no
/// conversation history, no workspace, and no second route.
pub trait Provider {
    /// Start exactly one request. Called at most once per run.
    fn begin(&mut self, request: &HelpRequest) -> Begin;
    /// Poll an attempt. May be called after `cancel`.
    fn poll(&mut self, ticket: Ticket, now_ms: u64) -> Poll;
    /// Ask the attempt to stop. Quiescence is learned from `poll`, not here.
    fn cancel(&mut self, ticket: Ticket);
}

/// Lifecycle of one ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Queued,
    Running,
    /// Stopping: cancelled, timed out, or finished, and the host has not yet
    /// observed the provider release its resources.
    Draining,
    Answered,
    Abstained,
    Denied,
    /// Cancelled *and* observed to have stopped.
    Cancelled,
    /// The provider never reached quiescence. Distinct from cancelled, and the
    /// capacity slot is still held.
    Abandoned,
    TimedOut,
}

impl RunState {
    /// Whether this state still occupies a capacity slot.
    #[must_use]
    pub const fn holds_capacity(self) -> bool {
        matches!(self, Self::Running | Self::Draining | Self::Abandoned)
    }

    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Answered
                | Self::Abstained
                | Self::Denied
                | Self::Cancelled
                | Self::Abandoned
                | Self::TimedOut
        )
    }
}

/// Durable identity for one ask, stable across a restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    pub run_id: String,
    /// Opaque handle the renderer holds.
    pub handle: String,
    pub session_token: String,
    pub epoch: u64,
}

/// One ask under supervision.
#[derive(Debug)]
pub struct Run {
    pub identity: RunIdentity,
    pub state: RunState,
    pub grant: Grant,
    pub admission: Admission,
    pub request: HelpRequest,
    pub ticket: Option<Ticket>,
    pub send_certainty: SendCertainty,
    pub reply: Option<String>,
    pub deny_reason: Option<DenyReason>,
    pub started_at_ms: u64,
    pub deadline_at_ms: u64,
    /// When draining began, for the abandon window.
    pub draining_since_ms: Option<u64>,
    pub finished_at_ms: Option<u64>,
    pub cancel_requested: bool,
}

impl Run {
    /// The coarse code a renderer may see for this run.
    #[must_use]
    pub fn public_code(&self) -> Option<PublicErrorCode> {
        match self.state {
            RunState::Denied => Some(
                self.deny_reason
                    .as_ref()
                    .map_or(PublicErrorCode::NotAvailable, |reason| {
                        DenyReason::public_code(reason)
                    }),
            ),
            RunState::TimedOut | RunState::Abandoned => Some(PublicErrorCode::Timeout),
            RunState::Cancelled => Some(PublicErrorCode::NotAvailable),
            _ => None,
        }
    }
}

/// Why a submission was refused before it became a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    Denied(DenyReason),
    Saturated,
}

/// The host-supervised executor.
pub struct Executor<P: Provider> {
    bounds: Bounds,
    provider: P,
    runs: Vec<Run>,
    queue: VecDeque<String>,
    next_id: u64,
    epoch: u64,
    provider_calls: usize,
}

impl<P: Provider> Executor<P> {
    #[must_use]
    pub fn new(bounds: Bounds, provider: P) -> Self {
        Self {
            bounds,
            provider,
            runs: Vec::new(),
            queue: VecDeque::new(),
            next_id: 1,
            epoch: 1,
            provider_calls: 0,
        }
    }

    #[must_use]
    pub const fn bounds(&self) -> &Bounds {
        &self.bounds
    }

    /// Total `Provider::begin` calls made. Never more than one per run.
    #[must_use]
    pub const fn provider_calls(&self) -> usize {
        self.provider_calls
    }

    #[must_use]
    pub fn provider(&self) -> &P {
        &self.provider
    }

    /// Slots currently held, including draining and abandoned runs.
    #[must_use]
    pub fn capacity_in_use(&self) -> usize {
        self.runs
            .iter()
            .filter(|run| run.state.holds_capacity())
            .count()
    }

    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    #[must_use]
    pub fn run(&self, handle: &str) -> Option<&Run> {
        self.runs.iter().find(|run| run.identity.handle == handle)
    }

    fn run_mut(&mut self, handle: &str) -> Option<&mut Run> {
        self.runs
            .iter_mut()
            .find(|run| run.identity.handle == handle)
    }

    fn mint(&mut self, prefix: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{prefix}-{id:08}")
    }

    /// Admit one ask into the queue.
    ///
    /// # Errors
    /// Returns [`SubmitError::Denied`] when authority refuses, or
    /// [`SubmitError::Saturated`] when the queue is full. Neither path touches
    /// the provider.
    pub fn submit(
        &mut self,
        authority: &mut Authority,
        session_token: &str,
        grant: Grant,
        request: HelpRequest,
        now_ms: u64,
    ) -> Result<RunIdentity, SubmitError> {
        if self.queue.len() >= self.bounds.max_queued {
            return Err(SubmitError::Saturated);
        }
        let deadline = now_ms.saturating_add(self.bounds.deadline_ms);
        let admission = authority
            .admit(session_token, &grant, &request, now_ms, deadline)
            .map_err(SubmitError::Denied)?;

        let identity = RunIdentity {
            run_id: self.mint("run"),
            handle: self.mint("help"),
            session_token: session_token.to_string(),
            epoch: self.epoch,
        };
        self.queue.push_back(identity.handle.clone());
        self.runs.push(Run {
            identity: identity.clone(),
            state: RunState::Queued,
            grant,
            admission,
            request,
            ticket: None,
            // Nothing has been sent, and this is guaranteed by construction:
            // `begin` is only reachable from `tick`, after promotion.
            send_certainty: SendCertainty::NotSent,
            reply: None,
            deny_reason: None,
            started_at_ms: now_ms,
            deadline_at_ms: deadline,
            draining_since_ms: None,
            finished_at_ms: None,
            cancel_requested: false,
        });
        Ok(identity)
    }

    /// Ask a run to stop.
    ///
    /// A queued run stops immediately: nothing was sent. A running one enters
    /// [`RunState::Draining`] and only becomes [`RunState::Cancelled`] once the
    /// provider is observed to have quiesced.
    pub fn cancel(&mut self, handle: &str, now_ms: u64) {
        let Some(index) = self
            .runs
            .iter()
            .position(|run| run.identity.handle == handle)
        else {
            return;
        };
        let ticket = {
            let run = &mut self.runs[index];
            if run.state.is_terminal() {
                return;
            }
            run.cancel_requested = true;
            match run.state {
                RunState::Queued => {
                    run.state = RunState::Cancelled;
                    run.finished_at_ms = Some(now_ms);
                    None
                }
                _ => {
                    run.state = RunState::Draining;
                    run.draining_since_ms.get_or_insert(now_ms);
                    run.ticket
                }
            }
        };
        self.queue.retain(|queued| queued != handle);
        if let Some(ticket) = ticket {
            self.provider.cancel(ticket);
        }
    }

    /// Cut every in-flight run, as a restart does.
    ///
    /// Runs that had reached the provider are *not* silently discarded: their
    /// send certainty is preserved, so a restart cannot turn a request that
    /// may have been delivered into one that was never sent. The epoch bump
    /// makes every pre-restart handle unrecognisable to the renderer.
    pub fn restart(&mut self, now_ms: u64) {
        self.epoch += 1;
        let handles: Vec<String> = self
            .runs
            .iter()
            .filter(|run| !run.state.is_terminal())
            .map(|run| run.identity.handle.clone())
            .collect();
        for handle in handles {
            self.cancel(&handle, now_ms);
        }
    }

    /// Advance the executor. Deterministic: no clocks, no sleeps.
    ///
    /// Order matters. Draining runs are settled first so their capacity is
    /// released before promotion considers the queue, and every promotion
    /// re-authorizes against current state rather than trusting admission.
    pub fn tick(&mut self, authority: &Authority, now_ms: u64) {
        self.settle_draining(now_ms);
        self.poll_running(authority, now_ms);
        self.promote(authority, now_ms);
    }

    fn settle_draining(&mut self, now_ms: u64) {
        for index in 0..self.runs.len() {
            if self.runs[index].state != RunState::Draining {
                continue;
            }
            let ticket = self.runs[index].ticket;
            let quiesced = match ticket {
                None => true,
                Some(ticket) => {
                    matches!(
                        self.provider.poll(ticket, now_ms),
                        Poll::Quiesced | Poll::Failed
                    )
                }
            };
            let run = &mut self.runs[index];
            if quiesced {
                // The provider really stopped, so the honest label is whatever
                // caused the drain.
                run.state = if run.cancel_requested {
                    RunState::Cancelled
                } else {
                    RunState::TimedOut
                };
                run.finished_at_ms = Some(now_ms);
                continue;
            }
            let since = run.draining_since_ms.unwrap_or(now_ms);
            if now_ms.saturating_sub(since) >= self.bounds.abandon_after_ms {
                // The provider is deaf. It was not cancelled — it was asked to
                // stop and did not, so the run is abandoned and its slot stays
                // held, because the remote work may still be running.
                run.state = RunState::Abandoned;
                run.finished_at_ms = Some(now_ms);
            }
        }
    }

    fn poll_running(&mut self, authority: &Authority, now_ms: u64) {
        for index in 0..self.runs.len() {
            if self.runs[index].state != RunState::Running {
                continue;
            }
            if now_ms >= self.runs[index].deadline_at_ms {
                let ticket = self.runs[index].ticket;
                let run = &mut self.runs[index];
                run.state = RunState::Draining;
                run.draining_since_ms.get_or_insert(now_ms);
                if let Some(ticket) = ticket {
                    self.provider.cancel(ticket);
                }
                continue;
            }
            let Some(ticket) = self.runs[index].ticket else {
                continue;
            };
            match self.provider.poll(ticket, now_ms) {
                Poll::Pending => {}
                Poll::Replied(reply) => {
                    // Reauthorize before serving. The answer exists, but the
                    // right to see it is re-decided here, not at admission.
                    let allowed = {
                        let run = &self.runs[index];
                        authority.reauthorize(
                            Checkpoint::BeforeServe,
                            &run.identity.session_token,
                            &run.grant,
                            Some(&run.admission),
                            Some(&run.request),
                            now_ms,
                        )
                    };
                    let run = &mut self.runs[index];
                    match allowed {
                        Ok(()) => {
                            run.reply = Some(reply);
                            run.state = RunState::Draining;
                            run.draining_since_ms.get_or_insert(now_ms);
                        }
                        Err(reason) => {
                            run.deny_reason = Some(reason);
                            run.state = RunState::Draining;
                            run.draining_since_ms.get_or_insert(now_ms);
                        }
                    }
                }
                Poll::Failed => {
                    let run = &mut self.runs[index];
                    run.state = RunState::Draining;
                    run.draining_since_ms.get_or_insert(now_ms);
                }
                Poll::Quiesced => {
                    let run = &mut self.runs[index];
                    run.state = RunState::Draining;
                    run.draining_since_ms.get_or_insert(now_ms);
                }
            }
        }
    }

    fn promote(&mut self, authority: &Authority, now_ms: u64) {
        while self.capacity_in_use() < self.bounds.max_concurrency {
            let Some(handle) = self.queue.pop_front() else {
                break;
            };
            let Some(index) = self
                .runs
                .iter()
                .position(|run| run.identity.handle == handle)
            else {
                continue;
            };
            if self.runs[index].state != RunState::Queued {
                continue;
            }

            // Checkpoint 2: promotion out of the queue.
            let promotion = {
                let run = &self.runs[index];
                authority.reauthorize(
                    Checkpoint::QueuePromotion,
                    &run.identity.session_token,
                    &run.grant,
                    Some(&run.admission),
                    Some(&run.request),
                    now_ms,
                )
            };
            if let Err(reason) = promotion {
                let run = &mut self.runs[index];
                run.deny_reason = Some(reason);
                run.state = RunState::Denied;
                run.finished_at_ms = Some(now_ms);
                continue;
            }

            // Checkpoint 3: immediately before the send. Nothing runs between
            // this check and `begin`.
            let before_send = {
                let run = &self.runs[index];
                authority.reauthorize(
                    Checkpoint::BeforeSend,
                    &run.identity.session_token,
                    &run.grant,
                    Some(&run.admission),
                    Some(&run.request),
                    now_ms,
                )
            };
            if let Err(reason) = before_send {
                let run = &mut self.runs[index];
                run.deny_reason = Some(reason);
                run.state = RunState::Denied;
                run.finished_at_ms = Some(now_ms);
                continue;
            }

            let request = self.runs[index].request.clone();
            self.provider_calls += 1;
            let begin = self.provider.begin(&request);
            let run = &mut self.runs[index];
            match begin {
                Begin::Accepted(ticket) => {
                    run.ticket = Some(ticket);
                    run.send_certainty = SendCertainty::Sent;
                    run.state = RunState::Running;
                }
                Begin::Uncertain(ticket) => {
                    run.ticket = Some(ticket);
                    run.send_certainty = SendCertainty::Unknown;
                    run.state = RunState::Running;
                }
                Begin::Rejected => {
                    // Nothing left the process, so `NotSent` remains true.
                    run.state = RunState::Draining;
                    run.draining_since_ms.get_or_insert(now_ms);
                }
            }
        }
    }

    /// Mark a drained run as answered or abstained once its reply is validated.
    pub fn settle_outcome(&mut self, handle: &str, answered: bool, now_ms: u64) {
        if let Some(run) = self.run_mut(handle) {
            run.state = if answered {
                RunState::Answered
            } else {
                RunState::Abstained
            };
            run.finished_at_ms.get_or_insert(now_ms);
        }
    }

    /// Build the zero-content receipt for a finished run.
    #[must_use]
    pub fn receipt(
        &self,
        handle: &str,
        principal_id: &str,
        tenant_id: &str,
        session_id: &str,
        claim_count: usize,
        span_count: usize,
        redactions: Vec<RedactionCount>,
        now_ms: u64,
    ) -> Option<Receipt> {
        let run = self.run(handle)?;
        let outcome = match run.state {
            RunState::Answered => Outcome::Answered,
            RunState::Abstained => Outcome::Abstained,
            RunState::Denied => Outcome::Denied,
            RunState::Cancelled => Outcome::Cancelled,
            RunState::Abandoned => Outcome::Abandoned,
            RunState::TimedOut => Outcome::TimedOut,
            // A run still in flight has no receipt to give.
            RunState::Queued | RunState::Running | RunState::Draining => return None,
        };
        let finished = run.finished_at_ms.unwrap_or(now_ms);
        let receipt_id = format!("receipt-{}", run.identity.run_id);
        let digest = Receipt::compute_digest(
            &receipt_id,
            &run.identity.run_id,
            &run.request.request_id,
            &run.request.digest,
            outcome,
            run.send_certainty,
            claim_count,
            span_count,
            finished,
        );
        Some(Receipt {
            receipt_id,
            run_id: run.identity.run_id.clone(),
            request_id: run.request.request_id.clone(),
            principal_id: principal_id.to_string(),
            tenant_id: tenant_id.to_string(),
            session_id: session_id.to_string(),
            corpus_digest: run.request.corpus_digest.clone(),
            manifest_revision: run.request.manifest_revision,
            request_digest: run.request.digest.clone(),
            outcome,
            send_certainty: run.send_certainty,
            deny_reason: run.deny_reason.clone(),
            public_code: run.public_code(),
            claim_count,
            span_count,
            redactions,
            started_at_ms: run.started_at_ms,
            finished_at_ms: finished,
            digest,
        })
    }
}
