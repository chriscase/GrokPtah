//! Deterministic fake host used by the headless-port tests.
//!
//! The fake is a *host*, not a mock of the port: it owns the same durable
//! shapes a real host owns — a write-ahead claim ledger, run records, a
//! bounded event journal with a retention floor, and a generation stamp — so
//! the tests exercise the port's real discipline rather than a rehearsal of
//! it. Nothing here reaches a clock, a filesystem, a provider, or a network:
//! every instant is supplied by the test.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;
use uuid::Uuid;

use super::authority::{EffectAuthorization, HeadlessAuthority, PortBinding, PortEventFacts};
use super::projection::PortEventKind;
use super::types::{
    scope_denied, HostNegotiation, PortClaimEvidence, PortClaimState, PortDeliveryEvidence,
    PortError, PortErrorCode, PortEvidenceSummary, PortExecutionMode, PortHostKind, PortLimits,
    PortOperation, PortPrincipal, PortPromotionState, PortResult, PortReviewFacts, PortRunFacts,
    PortRunState, PortStopCause, PortVerification, HEADLESS_PORT_PROTOCOL_VERSION,
};

pub(crate) fn instant(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_800_000_000 + seconds, 0)
        .single()
        .expect("fixed instant")
}

pub(crate) fn small_limits() -> PortLimits {
    PortLimits {
        max_prompt_bytes: 64,
        max_rounds: 2,
        max_duration_ms: 30_000,
        max_total_tokens: Some(4_000),
        max_event_page: 4,
    }
}

pub(crate) fn large_limits() -> PortLimits {
    PortLimits {
        max_prompt_bytes: 100_000,
        max_rounds: 24,
        max_duration_ms: 900_000,
        max_total_tokens: Some(2_000_000),
        max_event_page: 250,
    }
}

struct StoredRun {
    workspace: String,
    facts: PortRunFacts,
}

struct FakeState {
    host_id: String,
    host_kind: PortHostKind,
    capability_revision: u64,
    capabilities: BTreeSet<PortOperation>,
    limits: PortLimits,
    generation_started_at: DateTime<Utc>,
    now: DateTime<Utc>,
    session_id: Uuid,
    workspace: String,
    authorization_live: bool,
    negotiate_failure: Option<PortError>,
    /// Leave the write-ahead claim unsettled and fail the effect, exactly as a
    /// process death between the effect and its acknowledgement would.
    interrupt_next_submit: bool,
    /// Issue an authorization that does not describe the requested effect.
    misissue_authorization: bool,
    claims: BTreeMap<String, PortClaimEvidence>,
    runs: BTreeMap<String, StoredRun>,
    reviews: BTreeMap<String, PortReviewFacts>,
    journal: Vec<(String, u64, PortEventKind)>,
    retained_from_seq: u64,
    next_seq: u64,
    next_run: u64,
    performed_submits: u32,
    performed_cancels: u32,
    last_execution_mode: Option<PortExecutionMode>,
}

/// Deterministic in-memory host.
pub(crate) struct FakeHost {
    state: Mutex<FakeState>,
}

impl FakeHost {
    pub(crate) fn new(session_id: Uuid, workspace: &str) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                host_id: "fake-host".into(),
                host_kind: PortHostKind::Embedded,
                capability_revision: 7,
                capabilities: [
                    PortOperation::Submit,
                    PortOperation::Events,
                    PortOperation::Review,
                    PortOperation::Cancel,
                ]
                .into_iter()
                .collect(),
                limits: large_limits(),
                generation_started_at: instant(0),
                now: instant(10),
                session_id,
                workspace: workspace.to_string(),
                authorization_live: true,
                negotiate_failure: None,
                interrupt_next_submit: false,
                misissue_authorization: false,
                claims: BTreeMap::new(),
                runs: BTreeMap::new(),
                reviews: BTreeMap::new(),
                journal: Vec::new(),
                retained_from_seq: 1,
                next_seq: 1,
                next_run: 1,
                performed_submits: 0,
                performed_cancels: 0,
                last_execution_mode: None,
            }),
        })
    }

    pub(crate) fn authority(self: &Arc<Self>) -> FakeAuthority {
        FakeAuthority {
            host: Arc::clone(self),
        }
    }

    pub(crate) fn negotiation_snapshot(&self) -> HostNegotiation {
        let state = self.state.lock();
        HostNegotiation {
            protocol_version: HEADLESS_PORT_PROTOCOL_VERSION,
            host_id: state.host_id.clone(),
            host_kind: state.host_kind,
            capability_revision: state.capability_revision,
            capabilities: state.capabilities.clone(),
            limits: state.limits,
            generation_started_at: state.generation_started_at,
        }
    }

    pub(crate) fn set_limits(&self, limits: PortLimits) {
        let mut state = self.state.lock();
        state.limits = limits;
        // Any change to declared limits changes the capability revision.
        state.capability_revision += 1;
    }

    pub(crate) fn set_capabilities(&self, capabilities: impl IntoIterator<Item = PortOperation>) {
        let mut state = self.state.lock();
        state.capabilities = capabilities.into_iter().collect();
        state.capability_revision += 1;
    }

    pub(crate) fn misissue_authorization(&self) {
        self.state.lock().misissue_authorization = true;
    }

    pub(crate) fn revoke_authorization(&self) {
        self.state.lock().authorization_live = false;
    }

    pub(crate) fn fail_negotiation(&self, error: PortError) {
        self.state.lock().negotiate_failure = Some(error);
    }

    pub(crate) fn interrupt_next_submit(&self) {
        self.state.lock().interrupt_next_submit = true;
    }

    /// Simulate a process restart: a new generation begins, every unsettled
    /// write-ahead claim settles as interrupted, and every live run becomes
    /// interrupted. Nothing is replayed.
    pub(crate) fn restart(&self, generation_started_at: DateTime<Utc>) {
        let mut state = self.state.lock();
        state.generation_started_at = generation_started_at;
        state.now = generation_started_at;
        state.authorization_live = true;
        for claim in state.claims.values_mut() {
            if claim.state == PortClaimState::Claimed {
                claim.state = PortClaimState::FailedInterrupted;
                claim.rejection = Some(PortError::new(
                    PortErrorCode::Uncertain,
                    "claim was interrupted before its durable receipt completed",
                ));
            }
        }
        for stored in state.runs.values_mut() {
            if !stored.facts.state.is_terminal() {
                stored.facts.state = PortRunState::Interrupted;
                stored.facts.stop_cause = Some(PortStopCause::Interrupted);
            }
        }
    }

    /// Drop journal entries below `retained_from_seq`, as a bounded ring does.
    pub(crate) fn expire_journal_below(&self, retained_from_seq: u64) {
        let mut state = self.state.lock();
        state.retained_from_seq = retained_from_seq;
        state
            .journal
            .retain(|(_, seq, _)| *seq >= retained_from_seq);
    }

    pub(crate) fn append_events(&self, run_id: &str, kinds: &[PortEventKind]) {
        let mut state = self.state.lock();
        for kind in kinds {
            let seq = state.next_seq;
            state.next_seq += 1;
            state.journal.push((run_id.to_string(), seq, *kind));
            if let Some(stored) = state.runs.get_mut(run_id) {
                if stored.facts.start_seq.is_none() {
                    stored.facts.start_seq = Some(seq);
                }
                stored.facts.end_seq = None;
            }
        }
    }

    /// Move a run to a terminal state with an explicit evidence summary.
    pub(crate) fn finish_run(
        &self,
        run_id: &str,
        state_after: PortRunState,
        evidence: PortEvidenceSummary,
    ) {
        let mut state = self.state.lock();
        let last_seq = state
            .journal
            .iter()
            .filter(|(id, _, _)| id == run_id)
            .map(|(_, seq, _)| *seq)
            .max();
        if let Some(stored) = state.runs.get_mut(run_id) {
            stored.facts.state = state_after;
            stored.facts.evidence = evidence;
            stored.facts.end_seq = last_seq;
            stored.facts.stop_cause = Some(match state_after {
                PortRunState::Completed => PortStopCause::Completed,
                PortRunState::Cancelled => PortStopCause::Cancelled,
                PortRunState::Interrupted => PortStopCause::Interrupted,
                PortRunState::LimitReached => PortStopCause::RoundLimit,
                _ => PortStopCause::Failed,
            });
        }
    }

    pub(crate) fn set_review(&self, review: PortReviewFacts) {
        self.state
            .lock()
            .reviews
            .insert(review.run_id.clone(), review);
    }

    pub(crate) fn performed_submits(&self) -> u32 {
        self.state.lock().performed_submits
    }

    pub(crate) fn last_execution_mode(&self) -> Option<PortExecutionMode> {
        self.state.lock().last_execution_mode
    }

    pub(crate) fn performed_cancels(&self) -> u32 {
        self.state.lock().performed_cancels
    }

    pub(crate) fn run_ids(&self) -> Vec<String> {
        self.state.lock().runs.keys().cloned().collect()
    }

    /// Insert a run that no claim ever acknowledged, as a crash between the
    /// effect and the write of its receipt would leave behind.
    pub(crate) fn insert_orphan_run(&self, request_id: &str) -> String {
        let mut state = self.state.lock();
        let run = new_run(&mut state, request_id);
        let run_id = run.run_id.clone();
        let workspace = state.workspace.clone();
        state.runs.insert(
            run_id.clone(),
            StoredRun {
                workspace,
                facts: run,
            },
        );
        run_id
    }
}

fn new_run(state: &mut FakeState, request_id: &str) -> PortRunFacts {
    let run_id = format!("run-{}", state.next_run);
    state.next_run += 1;
    PortRunFacts {
        run_id,
        session_id: state.session_id,
        request_id: request_id.to_string(),
        state: PortRunState::Running,
        queued_position: None,
        start_seq: None,
        end_seq: None,
        round: 0,
        max_rounds: state.limits.max_rounds,
        admitted_limits: state.limits,
        evidence: PortEvidenceSummary {
            usage_complete: true,
            ..PortEvidenceSummary::default()
        },
        stop_cause: None,
        promotion: Some(PortPromotionState::NotApplicable),
        created_at: state.now,
        updated_at: state.now,
    }
}

/// Fully evidenced terminal completion.
pub(crate) fn verified_evidence() -> PortEvidenceSummary {
    PortEvidenceSummary {
        changed_files: 2,
        tests_observed: 1,
        tests_passed: 1,
        total_tokens: 1_234,
        provider_requests: 3,
        usage_complete: true,
        usage_pending_requests: 0,
        verification: Some(PortVerification::Verified),
        ..PortEvidenceSummary::default()
    }
}

#[derive(Clone)]
pub(crate) struct FakeAuthority {
    host: Arc<FakeHost>,
}

impl FakeAuthority {
    fn scoped(&self, binding: &PortBinding, state: &FakeState, run_id: &str) -> PortResult<()> {
        let stored = state.runs.get(run_id).ok_or_else(scope_denied)?;
        if stored.facts.session_id != binding.session_id()
            || stored.workspace != binding.workspace()
        {
            return Err(scope_denied());
        }
        Ok(())
    }
}

#[async_trait]
impl HeadlessAuthority for FakeAuthority {
    async fn negotiate(&self, _principal: &PortPrincipal) -> PortResult<HostNegotiation> {
        if let Some(error) = self.host.state.lock().negotiate_failure.clone() {
            return Err(error);
        }
        Ok(self.host.negotiation_snapshot())
    }

    async fn authorize_effect(
        &self,
        binding: &PortBinding,
        operation: PortOperation,
    ) -> PortResult<EffectAuthorization> {
        let state = self.host.state.lock();
        if !state.authorization_live
            || binding.session_id() != state.session_id
            || binding.workspace() != state.workspace
        {
            return Err(scope_denied());
        }
        let operation = if state.misissue_authorization {
            PortOperation::Events
        } else {
            operation
        };
        Ok(EffectAuthorization::issue(binding, operation, state.now))
    }

    async fn delivery_evidence(
        &self,
        binding: &PortBinding,
        request_id: &str,
    ) -> PortResult<PortDeliveryEvidence> {
        let state = self.host.state.lock();
        let run = state
            .runs
            .values()
            .find(|stored| {
                stored.facts.request_id == request_id
                    && stored.facts.session_id == binding.session_id()
                    && stored.workspace == binding.workspace()
            })
            .map(|stored| stored.facts.clone());
        Ok(PortDeliveryEvidence {
            claim: state.claims.get(request_id).cloned(),
            run,
        })
    }

    async fn perform_submit(
        &self,
        _binding: &PortBinding,
        _authorization: EffectAuthorization,
        request_id: &str,
        prompt: &str,
        limits: &PortLimits,
        execution_mode: PortExecutionMode,
        _allow_queue: bool,
    ) -> PortResult<PortRunFacts> {
        let mut state = self.host.state.lock();
        state.performed_submits += 1;
        state.last_execution_mode = Some(execution_mode);
        if prompt.len() > limits.max_prompt_bytes {
            return Err(PortError::new(
                PortErrorCode::LimitExceeded,
                "host refused an oversized prompt",
            ));
        }
        let now = state.now;
        // Write ahead of the effect.
        state.claims.insert(
            request_id.to_string(),
            PortClaimEvidence {
                state: PortClaimState::Claimed,
                operation: Some(PortOperation::Submit),
                claimed_at: now,
                run_id: None,
                queued_position: None,
                rejection: None,
            },
        );
        // Act.
        let mut facts = new_run(&mut state, request_id);
        facts.admitted_limits = *limits;
        facts.max_rounds = limits.max_rounds;
        let run_id = facts.run_id.clone();
        let workspace = state.workspace.clone();
        state.runs.insert(
            run_id.clone(),
            StoredRun {
                workspace,
                facts: facts.clone(),
            },
        );
        if state.interrupt_next_submit {
            state.interrupt_next_submit = false;
            // The effect landed; the acknowledgement never will.
            return Err(PortError::new(
                PortErrorCode::Internal,
                "host was interrupted before acknowledging the effect",
            ));
        }
        // Acknowledge.
        if let Some(claim) = state.claims.get_mut(request_id) {
            claim.state = PortClaimState::Completed;
            claim.run_id = Some(run_id);
        }
        Ok(facts)
    }

    async fn perform_cancel(
        &self,
        binding: &PortBinding,
        _authorization: EffectAuthorization,
        request_id: &str,
        run_id: &str,
    ) -> PortResult<PortRunFacts> {
        let mut state = self.host.state.lock();
        state.performed_cancels += 1;
        self.scoped(binding, &state, run_id)?;
        let now = state.now;
        state.claims.insert(
            request_id.to_string(),
            PortClaimEvidence {
                state: PortClaimState::Claimed,
                operation: Some(PortOperation::Cancel),
                claimed_at: now,
                run_id: Some(run_id.to_string()),
                queued_position: None,
                rejection: None,
            },
        );
        let stored = state.runs.get_mut(run_id).ok_or_else(scope_denied)?;
        if !stored.facts.state.is_terminal() {
            stored.facts.state = PortRunState::Cancelled;
            stored.facts.stop_cause = Some(PortStopCause::Cancelled);
            stored.facts.updated_at = now;
        }
        let facts = stored.facts.clone();
        if let Some(claim) = state.claims.get_mut(request_id) {
            claim.state = PortClaimState::Completed;
        }
        Ok(facts)
    }

    async fn run_facts(&self, binding: &PortBinding, run_id: &str) -> PortResult<PortRunFacts> {
        let state = self.host.state.lock();
        self.scoped(binding, &state, run_id)?;
        Ok(state.runs.get(run_id).expect("scoped run").facts.clone())
    }

    async fn run_events(
        &self,
        binding: &PortBinding,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> PortResult<PortEventFacts> {
        let state = self.host.state.lock();
        self.scoped(binding, &state, run_id)?;
        let facts = &state.runs.get(run_id).expect("scoped run").facts;
        // A cursor below the retained window is a gap, not a short page.
        if after_seq + 1 < state.retained_from_seq {
            return Ok(PortEventFacts {
                entries: Vec::new(),
                next_cursor: None,
                cursor_expired: true,
                start_seq: facts.start_seq,
                end_seq: facts.end_seq,
            });
        }
        let mut entries: Vec<(u64, PortEventKind)> = state
            .journal
            .iter()
            .filter(|(id, seq, _)| id == run_id && *seq > after_seq)
            .map(|(_, seq, kind)| (*seq, *kind))
            .collect();
        entries.sort_unstable_by_key(|(seq, _)| *seq);
        let more = entries.len() > limit;
        entries.truncate(limit);
        let next_cursor = more.then(|| entries.last().map(|(seq, _)| *seq)).flatten();
        Ok(PortEventFacts {
            entries,
            next_cursor,
            cursor_expired: false,
            start_seq: facts.start_seq,
            end_seq: facts.end_seq,
        })
    }

    async fn review_facts(
        &self,
        binding: &PortBinding,
        run_id: &str,
    ) -> PortResult<PortReviewFacts> {
        let state = self.host.state.lock();
        self.scoped(binding, &state, run_id)?;
        state.reviews.get(run_id).cloned().ok_or_else(scope_denied)
    }
}
