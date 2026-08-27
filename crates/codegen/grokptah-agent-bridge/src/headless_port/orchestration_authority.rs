//! Production adapter: the headless port over the existing orchestration
//! control plane.
//!
//! This adapter contains no send engine. Every effect is delegated to the
//! `OrchestrationService` methods the desktop and `grokptah-service` hosts
//! already use — `submit_task_with_execution_mode_and_queue`, `cancel`,
//! `get_events_scoped`, `review_run` — and every durable fact is read back
//! from the same `OrchStore` records those methods write. What the adapter
//! adds is the mapping into the port's redaction-safe shapes and the
//! classification of the runtime's write-ahead idempotency ledger into the
//! port's visible delivery states.
//!
//! Authentication stays with the host. The adapter is constructed with an
//! already-authenticated [`AuthContext`]; a binding whose principal does not
//! match that context is refused rather than trusted.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::orchestration::{
    AuthContext, OrchError, OrchErrorCode, OrchestrationService, PromotionState, RunBounds,
    RunExecutionMode, RunRecord, RunState, RunStopCause, INTERRUPTED_CLAIM_DETAIL,
};

use super::authority::{EffectAuthorization, HeadlessAuthority, PortBinding, PortEventFacts};
use super::projection::classify_update;
use super::types::{
    scope_denied, HostNegotiation, PortClaimEvidence, PortClaimState, PortDeliveryEvidence,
    PortError, PortErrorCode, PortEvidenceSummary, PortExecutionMode, PortHostKind, PortLimits,
    PortOperation, PortPrincipal, PortPromotionState, PortResult, PortReviewFacts, PortRunFacts,
    PortRunState, PortStopCause, PortVerification, HEADLESS_PORT_PROTOCOL_VERSION,
    MAX_PORT_EVENT_PAGE,
};

/// Headless port authority backed by the shipped orchestration runtime.
pub struct OrchestrationAuthority {
    service: Arc<OrchestrationService>,
    auth: AuthContext,
    host_id: String,
    host_kind: PortHostKind,
    /// Instant this host generation began serving. Claims stamped before it
    /// belong to a generation that is gone.
    generation_started_at: DateTime<Utc>,
}

impl OrchestrationAuthority {
    /// `auth` must already be authenticated by the host; the port does not
    /// authenticate. `host_id` identifies this runtime home owner.
    pub fn new(
        service: Arc<OrchestrationService>,
        auth: AuthContext,
        host_id: impl Into<String>,
        host_kind: PortHostKind,
        generation_started_at: DateTime<Utc>,
    ) -> PortResult<Self> {
        Ok(Self {
            service,
            auth,
            host_id: super::types::validate_identifier(host_id.into())?,
            host_kind,
            generation_started_at,
        })
    }

    /// Every declared capability at this revision. The orchestration control
    /// plane implements all four operations.
    fn capabilities() -> std::collections::BTreeSet<PortOperation> {
        [
            PortOperation::Submit,
            PortOperation::Events,
            PortOperation::Review,
            PortOperation::Cancel,
        ]
        .into_iter()
        .collect()
    }

    fn limits(&self) -> PortLimits {
        limits_from_bounds(&self.service.bounds_ceiling())
    }

    /// The principal is a requested identity; it must match the credential the
    /// host already authenticated.
    fn require_principal(&self, principal: &PortPrincipal) -> PortResult<()> {
        if principal.credential_id != self.auth.token_id
            || principal.principal_id != self.auth.owner_id
        {
            return Err(PortError::new(
                PortErrorCode::Unauthenticated,
                "principal does not match the authenticated host credential",
            ));
        }
        Ok(())
    }

    fn workspace_path(binding: &PortBinding) -> PathBuf {
        PathBuf::from(binding.workspace())
    }

    /// Load a run through the runtime's own scope gate, then verify the
    /// binding's exact workspace string. Every failure collapses into the one
    /// scope error.
    fn scoped_run(&self, binding: &PortBinding, run_id: &str) -> PortResult<RunRecord> {
        let workspace = Self::workspace_path(binding);
        let run = self
            .service
            .authorize_run_request(binding.session_id(), &workspace, run_id)
            .map_err(|_| scope_denied())?;
        if run.workspace != binding.workspace() {
            return Err(scope_denied());
        }
        Ok(run)
    }
}

#[async_trait]
impl HeadlessAuthority for OrchestrationAuthority {
    async fn negotiate(&self, principal: &PortPrincipal) -> PortResult<HostNegotiation> {
        self.require_principal(principal)?;
        let capabilities = Self::capabilities();
        let limits = self.limits();
        Ok(HostNegotiation {
            protocol_version: HEADLESS_PORT_PROTOCOL_VERSION,
            host_id: self.host_id.clone(),
            host_kind: self.host_kind,
            capability_revision: capability_revision(&self.host_id, &capabilities, &limits),
            capabilities,
            limits,
            generation_started_at: self.generation_started_at,
        })
    }

    async fn authorize_effect(
        &self,
        binding: &PortBinding,
        operation: PortOperation,
    ) -> PortResult<EffectAuthorization> {
        self.require_principal(binding.principal())?;
        // Negotiation is not authorization: re-run the Build-session and
        // allowlisted-workspace gate right now, and require the binding's
        // exact workspace identity to still be the canonical one.
        let claimed = self
            .service
            .recheck_build_scope(binding.session_id(), &Self::workspace_path(binding))
            .map_err(map_scope_error)?;
        if claimed.display().to_string() != binding.workspace() {
            return Err(scope_denied());
        }
        Ok(EffectAuthorization::issue(binding, operation, Utc::now()))
    }

    async fn delivery_evidence(
        &self,
        binding: &PortBinding,
        request_id: &str,
    ) -> PortResult<PortDeliveryEvidence> {
        self.require_principal(binding.principal())?;
        let store = self.service.store();
        let receipt = store
            .load_idempotency(request_id)
            .map_err(|_| internal("could not read the durable idempotency ledger"))?;
        let claim = receipt.map(|receipt| {
            let state = match receipt.status.as_str() {
                "complete" => PortClaimState::Completed,
                "failed" => {
                    let interrupted = receipt
                        .error
                        .as_ref()
                        .is_some_and(|error| error.message == INTERRUPTED_CLAIM_DETAIL);
                    if interrupted {
                        PortClaimState::FailedInterrupted
                    } else {
                        PortClaimState::FailedRejected
                    }
                }
                _ => PortClaimState::Claimed,
            };
            PortClaimEvidence {
                state,
                operation: claim_operation(&receipt.tool),
                claimed_at: receipt.created_at,
                run_id: receipt.run_id.clone(),
                queued_position: None,
                rejection: receipt.error.as_ref().map(map_rejection),
            }
        });

        // A run durably attributed to this request id inside the binding's
        // scope is what turns an unsettled claim into evidence that the effect
        // may already have landed. A settled receipt names its run directly;
        // only an unsettled or unacknowledged claim needs the ledger scan, and
        // that scan is bounded by run retention.
        let named = claim.as_ref().and_then(|claim| claim.run_id.clone());
        let record = match named {
            Some(run_id) => store
                .load_run(&run_id)
                .map_err(|_| internal("could not read the durable run ledger"))?,
            None => store
                .list_runs()
                .map_err(|_| internal("could not read the durable run ledger"))?
                .into_iter()
                .find(|run| run.request_id == request_id),
        };
        let run = record
            .filter(|run| {
                run.session_id == binding.session_id() && run.workspace == binding.workspace()
            })
            .map(|run| map_run_facts(&run, &self.limits()));

        Ok(PortDeliveryEvidence { claim, run })
    }

    async fn perform_submit(
        &self,
        binding: &PortBinding,
        authorization: EffectAuthorization,
        request_id: &str,
        prompt: &str,
        limits: &PortLimits,
        execution_mode: PortExecutionMode,
        allow_queue: bool,
    ) -> PortResult<PortRunFacts> {
        debug_assert_eq!(authorization.operation(), PortOperation::Submit);
        self.require_principal(binding.principal())?;
        let response = self
            .service
            .submit_task_with_execution_mode_and_queue(
                &self.auth,
                request_id,
                binding.session_id(),
                &Self::workspace_path(binding),
                prompt.to_string(),
                Some(bounds_json(limits)),
                match execution_mode {
                    PortExecutionMode::Shared => RunExecutionMode::Shared,
                    PortExecutionMode::IsolatedWorktree => RunExecutionMode::IsolatedWorktree,
                },
                allow_queue,
            )
            .await
            .map_err(map_orch_error)?;
        let run_id = response
            .get("runId")
            .and_then(|value| value.as_str())
            .ok_or_else(|| internal("submit receipt did not name a run"))?
            .to_string();
        let run = self.scoped_run(binding, &run_id)?;
        Ok(map_run_facts(&run, limits))
    }

    async fn perform_cancel(
        &self,
        binding: &PortBinding,
        authorization: EffectAuthorization,
        request_id: &str,
        run_id: &str,
    ) -> PortResult<PortRunFacts> {
        debug_assert_eq!(authorization.operation(), PortOperation::Cancel);
        self.require_principal(binding.principal())?;
        self.service
            .cancel(
                &self.auth,
                request_id,
                binding.session_id(),
                &Self::workspace_path(binding),
                Some(run_id),
            )
            .await
            .map_err(map_orch_error)?;
        let run = self.scoped_run(binding, run_id)?;
        Ok(map_run_facts(&run, &self.limits()))
    }

    async fn run_facts(&self, binding: &PortBinding, run_id: &str) -> PortResult<PortRunFacts> {
        self.require_principal(binding.principal())?;
        let run = self.scoped_run(binding, run_id)?;
        Ok(map_run_facts(&run, &self.limits()))
    }

    async fn run_events(
        &self,
        binding: &PortBinding,
        run_id: &str,
        after_seq: u64,
        limit: usize,
    ) -> PortResult<PortEventFacts> {
        self.require_principal(binding.principal())?;
        let run = self.scoped_run(binding, run_id)?;
        let value = match self.service.get_events_scoped(
            &self.auth,
            binding.session_id(),
            &Self::workspace_path(binding),
            run_id,
            after_seq,
            limit,
        ) {
            Ok(value) => value,
            Err(error) if error.code == OrchErrorCode::CursorExpired => {
                // A gap is reported, never presented as a complete stream.
                return Ok(PortEventFacts {
                    entries: Vec::new(),
                    next_cursor: None,
                    cursor_expired: true,
                    start_seq: run.start_seq,
                    end_seq: run.end_seq,
                });
            }
            Err(error) => return Err(map_orch_error(error)),
        };
        let page: crate::event_bus::JournalPage = serde_json::from_value(value)
            .map_err(|_| internal("could not decode the durable event page"))?;
        if page.cursor_expired {
            return Ok(PortEventFacts {
                entries: Vec::new(),
                next_cursor: None,
                cursor_expired: true,
                start_seq: run.start_seq,
                end_seq: run.end_seq,
            });
        }
        // Classification is the redaction boundary: only the sequence and a
        // unit-variant kind survive it.
        let entries: Vec<(u64, super::projection::PortEventKind)> = page
            .entries
            .iter()
            .map(|entry| (entry.seq, classify_update(&entry.update)))
            .collect();
        // The runtime page always names its last sequence; the port contract
        // reserves `next_cursor` for "more may remain", which is only knowable
        // here from a full page.
        let next_cursor = (entries.len() >= limit)
            .then(|| entries.last().map(|(seq, _)| *seq))
            .flatten();
        Ok(PortEventFacts {
            entries,
            next_cursor,
            cursor_expired: false,
            start_seq: run.start_seq,
            end_seq: run.end_seq,
        })
    }

    async fn review_facts(
        &self,
        binding: &PortBinding,
        run_id: &str,
    ) -> PortResult<PortReviewFacts> {
        self.require_principal(binding.principal())?;
        let run = self.scoped_run(binding, run_id)?;
        let value = self
            .service
            .review_run(
                &self.auth,
                binding.session_id(),
                &Self::workspace_path(binding),
                run_id,
            )
            .map_err(map_orch_error)?;
        // Only counts, fingerprints, and typed state cross the port. The diff
        // body and the changed paths in this response are deliberately dropped
        // here rather than forwarded.
        let changed_file_count = value
            .get("changedFiles")
            .and_then(|files| files.as_array())
            .map(|files| files.len() as u32)
            .unwrap_or(0);
        let diff_available = value
            .get("diff")
            .and_then(|diff| diff.as_str())
            .is_some_and(|diff| !diff.is_empty());
        Ok(PortReviewFacts {
            run_id: run.run_id.clone(),
            promotion: run
                .execution
                .as_ref()
                .map(|execution| promotion_state(execution.promotion_state))
                .unwrap_or(PortPromotionState::NotApplicable),
            source_fingerprint: value
                .get("sourceFingerprint")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            final_fingerprint: value
                .get("finalFingerprint")
                .and_then(|value| value.as_str())
                .map(str::to_string),
            changed_file_count,
            diff_available,
            diff_truncated: value
                .get("diffTruncated")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }
}

/// Content-derived capability revision.
///
/// Deriving it from the declared capabilities and limits rather than tracking
/// a counter means the revision cannot fail to change when the contract does,
/// so a binding minted under a wider limit is always detected as stale.
fn capability_revision(
    host_id: &str,
    capabilities: &std::collections::BTreeSet<PortOperation>,
    limits: &PortLimits,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(super::types::HEADLESS_PORT_SCHEMA.as_bytes());
    hasher.update([0]);
    hasher.update(host_id.as_bytes());
    hasher.update([0]);
    for capability in capabilities {
        hasher.update(capability.as_str().as_bytes());
        hasher.update([0]);
    }
    hasher.update(
        format!(
            "{}:{}:{}:{}:{}",
            limits.max_prompt_bytes,
            limits.max_rounds,
            limits.max_duration_ms,
            limits.max_total_tokens.unwrap_or(0),
            limits.max_event_page,
        )
        .as_bytes(),
    );
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(bytes)
}

/// Map the runtime's idempotency tool name onto a port operation. An
/// unrecognized tool yields `None`, which the port treats as "not recorded"
/// rather than as a match.
fn claim_operation(tool: &str) -> Option<PortOperation> {
    match tool {
        "ptah_submit_task" => Some(PortOperation::Submit),
        "ptah_cancel" => Some(PortOperation::Cancel),
        _ => None,
    }
}

fn limits_from_bounds(bounds: &RunBounds) -> PortLimits {
    PortLimits {
        max_prompt_bytes: bounds.max_prompt_bytes,
        max_rounds: bounds.max_rounds,
        max_duration_ms: bounds.max_duration_ms,
        max_total_tokens: bounds.max_total_tokens,
        max_event_page: MAX_PORT_EVENT_PAGE,
    }
}

fn bounds_json(limits: &PortLimits) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("maxPromptBytes".into(), limits.max_prompt_bytes.into());
    map.insert("maxRounds".into(), limits.max_rounds.into());
    map.insert("maxDurationMs".into(), limits.max_duration_ms.into());
    if let Some(max_total_tokens) = limits.max_total_tokens {
        map.insert("maxTotalTokens".into(), max_total_tokens.into());
    }
    serde_json::Value::Object(map)
}

/// Map one durable run record into the port's already-redacted facts.
///
/// The prompt preview, the final model response, the workspace path, and the
/// changed file paths are read here and deliberately left behind: the target
/// type has nowhere to put them.
pub(crate) fn map_run_facts(run: &RunRecord, limits: &PortLimits) -> PortRunFacts {
    let verification = run.aggregates.verification.as_ref();
    let observations = verification.map(|evidence| &evidence.observations);
    let tests_observed = observations
        .map(|observations| observations.tests_observed)
        .unwrap_or(run.aggregates.tests.len() as u32);
    let tests_passed = observations
        .map(|observations| observations.tests_passed)
        .unwrap_or_else(|| {
            run.aggregates
                .tests
                .iter()
                .filter(|test| test.exit_code == Some(0) && test.cancelled != Some(true))
                .count() as u32
        });
    let tests_failed = observations
        .map(|observations| observations.tests_failed)
        .unwrap_or_else(|| {
            run.aggregates
                .tests
                .iter()
                .filter(|test| test.exit_code.is_some_and(|code| code != 0))
                .count() as u32
        });
    let tests_incomplete = observations
        .map(|observations| observations.tests_incomplete)
        .unwrap_or_else(|| {
            run.aggregates
                .tests
                .iter()
                .filter(|test| test.exit_code.is_none() || test.cancelled == Some(true))
                .count() as u32
        });
    PortRunFacts {
        run_id: run.run_id.clone(),
        session_id: run.session_id,
        request_id: run.request_id.clone(),
        state: run_state(run.state),
        queued_position: run.queue_position.map(|position| position as u32),
        start_seq: run.start_seq,
        end_seq: run.end_seq,
        round: run.progress.as_ref().map(|p| p.round).unwrap_or(0),
        max_rounds: run
            .progress
            .as_ref()
            .map(|p| p.max_rounds)
            .unwrap_or(run.bounds.max_rounds),
        admitted_limits: PortLimits {
            max_prompt_bytes: run.bounds.max_prompt_bytes,
            max_rounds: run.bounds.max_rounds,
            max_duration_ms: run.bounds.max_duration_ms,
            max_total_tokens: run.bounds.max_total_tokens,
            max_event_page: limits.max_event_page,
        },
        evidence: PortEvidenceSummary {
            changed_files: run.aggregates.changes.len() as u32,
            tests_observed,
            tests_passed,
            tests_failed,
            tests_incomplete,
            permissions_requested: run.aggregates.permissions_requested,
            permissions_granted: run.aggregates.permissions_granted,
            permissions_denied: run.aggregates.permissions_denied,
            total_tokens: run.aggregates.usage.total_tokens,
            provider_requests: run.aggregates.usage.requests,
            usage_complete: run.aggregates.usage_complete,
            usage_pending_requests: run.aggregates.usage_pending_requests,
            verification: verification.map(|evidence| verification_state(&evidence.status)),
        },
        stop_cause: run.stop_cause.map(stop_cause),
        promotion: run
            .execution
            .as_ref()
            .map(|execution| promotion_state(execution.promotion_state)),
        created_at: run.created_at,
        updated_at: run.updated_at,
    }
}

fn run_state(state: RunState) -> PortRunState {
    match state {
        RunState::Queued => PortRunState::Queued,
        RunState::Running => PortRunState::Running,
        RunState::Completed => PortRunState::Completed,
        RunState::Failed => PortRunState::Failed,
        RunState::Cancelled => PortRunState::Cancelled,
        RunState::Interrupted => PortRunState::Interrupted,
        RunState::LimitReached => PortRunState::LimitReached,
    }
}

fn stop_cause(cause: RunStopCause) -> PortStopCause {
    match cause {
        RunStopCause::Completed => PortStopCause::Completed,
        RunStopCause::RoundLimit => PortStopCause::RoundLimit,
        RunStopCause::DurationLimit => PortStopCause::DurationLimit,
        RunStopCause::TokenCeiling => PortStopCause::TokenCeiling,
        RunStopCause::TokenAccountingUnavailable => PortStopCause::TokenAccountingUnavailable,
        RunStopCause::TokenAccountingOverflow => PortStopCause::TokenAccountingOverflow,
        RunStopCause::Stationarity => PortStopCause::Stationarity,
        RunStopCause::RecoveryExhausted => PortStopCause::RecoveryExhausted,
        RunStopCause::Cancelled => PortStopCause::Cancelled,
        RunStopCause::Interrupted => PortStopCause::Interrupted,
        RunStopCause::Failed => PortStopCause::Failed,
    }
}

fn promotion_state(state: PromotionState) -> PortPromotionState {
    match state {
        PromotionState::NotApplicable => PortPromotionState::NotApplicable,
        PromotionState::Preparing => PortPromotionState::Preparing,
        PromotionState::Ready => PortPromotionState::Ready,
        PromotionState::Promoted => PortPromotionState::Promoted,
        PromotionState::Conflicted => PortPromotionState::Conflicted,
        PromotionState::Discarded => PortPromotionState::Discarded,
    }
}

/// The runtime records verification status as a closed set of strings. An
/// unrecognized value fails closed as `incomplete` rather than as verified.
fn verification_state(status: &str) -> PortVerification {
    match status {
        "verified" => PortVerification::Verified,
        "unverified" => PortVerification::Unverified,
        "failed" => PortVerification::Failed,
        _ => PortVerification::Incomplete,
    }
}

/// Map a runtime failure into a typed port failure with fixed host-authored
/// text. The runtime message is dropped, not forwarded: it can contain paths
/// and provider detail that must not cross the port.
fn map_orch_error(error: OrchError) -> PortError {
    match error.code {
        OrchErrorCode::Unauthenticated => PortError::new(
            PortErrorCode::Unauthenticated,
            "host refused the credential",
        ),
        OrchErrorCode::ForbiddenScope | OrchErrorCode::WorkspaceMismatch => scope_denied(),
        OrchErrorCode::SessionBusy | OrchErrorCode::CapacityExhausted => PortError::new(
            PortErrorCode::Unavailable,
            "host has no admission capacity for this session right now",
        ),
        OrchErrorCode::StaleVersion => PortError::new(
            PortErrorCode::StaleBinding,
            "host state moved after the request was prepared",
        ),
        OrchErrorCode::CursorExpired => PortError::new(
            PortErrorCode::CursorExpired,
            "event cursor is below the retained journal window",
        ),
        OrchErrorCode::Timeout => {
            PortError::new(PortErrorCode::Unavailable, "host request deadline exceeded")
        }
        OrchErrorCode::InvalidRequest => {
            PortError::new(PortErrorCode::InvalidRequest, "host rejected the request")
        }
        OrchErrorCode::Unsupported => PortError::new(
            PortErrorCode::Unsupported,
            "host does not support this operation",
        ),
        OrchErrorCode::Conflict => PortError::new(
            PortErrorCode::Conflict,
            "a durable claim for this request id is still in progress",
        ),
        OrchErrorCode::Internal => internal("host reported an internal failure"),
    }
}

fn map_scope_error(_error: OrchError) -> PortError {
    scope_denied()
}

/// A durable rejection recorded by the runtime, restated in fixed text.
fn map_rejection(error: &OrchError) -> PortError {
    map_orch_error(error.clone())
}

fn internal(message: &'static str) -> PortError {
    PortError::new(PortErrorCode::Internal, message)
}

/// Convenience constructor mirroring how a host builds the port.
pub fn orchestration_port(
    service: Arc<OrchestrationService>,
    auth: AuthContext,
    host_id: impl Into<String>,
    host_kind: PortHostKind,
    generation_started_at: DateTime<Utc>,
) -> PortResult<super::port::HeadlessAgentPort<OrchestrationAuthority>> {
    Ok(super::port::HeadlessAgentPort::new(
        OrchestrationAuthority::new(service, auth, host_id, host_kind, generation_started_at)?,
    ))
}
