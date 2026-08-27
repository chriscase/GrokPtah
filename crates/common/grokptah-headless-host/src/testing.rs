//! Deterministic fixtures shared by unit and contract tests.
//!
//! Everything here is offline and synthetic: no provider credential, no
//! network, no real workspace. The paths are built from the platform temporary
//! directory so the fixtures stay absolute on every supported platform without
//! creating anything on disk.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use grokptah_agent_sdk::run::ExecutionMode;
use grokptah_agent_sdk::{
    CONTRACT_VERSION, CapabilityAvailability, CapabilityDescriptor, CapabilitySet, CapabilityTier,
};

use crate::authority::ResolvedBounds;
use crate::authority::{CAP_EXECUTE, CAP_OBSERVE, CAP_PROMOTE, CAP_QUEUE, CAP_RESUME, CAP_REVIEW};
use crate::config::{EngineSelection, HostConfig, HostLimits};
use crate::engine::{DispatchDisposition, EngineOutcome};
use crate::identity::ExternalRef;
use crate::orchestration::{
    OrchestratorBinding, TurnOrchestrator, TurnReceipt, TurnRefusal, TurnRequest,
};
use crate::store::{RunPhase, RunRecord};

/// Fixed timestamp used by record fixtures.
pub const TS: &str = "2026-01-01T00:00:00.000Z";
/// Fixed epoch millisecond used by clock fixtures.
pub const NOW_MS: u64 = 1_767_225_600_000;

fn descriptor(
    id: &str,
    tier: CapabilityTier,
    mutating: bool,
    human_gate: bool,
    availability: CapabilityAvailability,
) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.to_owned(),
        tier,
        mutating,
        human_gate,
        availability,
        description: format!("fixture capability {id}"),
    }
}

/// A capability set covering every availability the host must handle:
/// available, gated, and unavailable.
pub fn capability_fixture() -> CapabilitySet {
    CapabilitySet {
        contract: CONTRACT_VERSION.to_owned(),
        capabilities: vec![
            descriptor(
                CAP_OBSERVE,
                CapabilityTier::Observe,
                false,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_EXECUTE,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_QUEUE,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_REVIEW,
                CapabilityTier::Review,
                false,
                false,
                CapabilityAvailability::Available,
            ),
            descriptor(
                CAP_PROMOTE,
                CapabilityTier::Promote,
                true,
                true,
                CapabilityAvailability::Gated,
            ),
            descriptor(
                CAP_RESUME,
                CapabilityTier::Execute,
                true,
                false,
                CapabilityAvailability::Unavailable,
            ),
        ],
    }
}

/// Root under which fixture paths are built. Nothing is created here.
pub fn fixture_root() -> PathBuf {
    std::env::temp_dir().join("grokptah-headless-fixture")
}

/// A validated configuration over synthetic sibling roots.
pub fn config_fixture() -> HostConfig {
    let root = fixture_root();
    config_for(&root.join("host-home"), &root.join("project"))
}

/// A validated configuration over explicit roots.
pub fn config_for(home: &Path, workspace: &Path) -> HostConfig {
    HostConfig {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        session_id: "session-fixture".to_owned(),
        capabilities: capability_fixture(),
        grants: Vec::new(),
        limits: HostLimits {
            max_active_runs: 1,
            max_queued_runs: 2,
            max_prompt_bytes: 4_096,
            max_rounds: 4,
            max_duration_ms: 60_000,
            event_retention: 32,
            max_event_bytes: 8_192,
            lease_ttl_ms: 1_000,
            attention_ttl_ms: 5_000,
        },
        engine: EngineSelection::Disabled,
    }
}

/// A durable run record fixture in an exact phase.
pub fn run_record_fixture(run_id: &str, phase: RunPhase) -> RunRecord {
    RunRecord {
        run_id: run_id.to_owned(),
        session_id: "session-fixture".to_owned(),
        workspace: "project".to_owned(),
        request_id: format!("req-{run_id}"),
        phase,
        prompt_preview: "build".to_owned(),
        request_fingerprint: "fingerprint-request".to_owned(),
        created_at: TS.to_owned(),
        updated_at: TS.to_owned(),
        revision: 1,
        rounds_used: 0,
        bounds: ResolvedBounds {
            max_prompt_bytes: 4_096,
            max_rounds: 4,
            max_duration_ms: 60_000,
        },
        execution_mode: ExecutionMode::IsolatedWorktree,
        started_at_ms: None,
        pending_steering: Vec::new(),
        attention: None,
        stop_reason: None,
        completion: None,
        dispatch: None,
    }
}

/// One turn the host asked a [`FakeOrchestrator`] to run.
///
/// Recorded so a test can assert the thing that matters most about an
/// orchestrated host: that a given dispatch ordinal is never handed out twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchLogEntry {
    /// Run the turn belonged to.
    pub run_id: String,
    /// Dispatch ordinal the host had already made durable.
    pub ordinal: u32,
    /// Round within the run.
    pub round: u16,
    /// Whether cancellation had been requested when the turn started.
    pub cancelled: bool,
}

/// Shared record of every turn a fake orchestrator was asked to run.
///
/// Clonable and shared, because the orchestrator is moved into the host and a
/// test still needs to see what it was asked to do.
#[derive(Debug, Clone, Default)]
pub struct DispatchLog {
    entries: Arc<Mutex<Vec<DispatchLogEntry>>>,
}

impl DispatchLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything recorded so far, in order.
    pub fn entries(&self) -> Vec<DispatchLogEntry> {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The dispatch ordinals seen so far, in order.
    pub fn ordinals(&self) -> Vec<u32> {
        self.entries().iter().map(|entry| entry.ordinal).collect()
    }

    fn push(&self, entry: DispatchLogEntry) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry);
    }
}

/// One scripted orchestrator turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeTurn {
    /// Return this receipt.
    Receipt(TurnReceipt),
    /// Refuse the turn.
    Refusal(TurnRefusal),
    /// Dispatch, then lose the answer: the classic case where retrying could
    /// repeat work that already happened.
    LostAfterDispatch {
        /// Opaque attempt reference the orchestrator managed to record.
        attempt: String,
    },
    /// A turn long enough for a stop to arrive after the request went out.
    ///
    /// The turn trips the shared cancel signal itself. The host is
    /// single-threaded, so that is how a stop arriving *during* a turn is
    /// modelled without sleeping, threads, or a second runtime — and it is
    /// exactly what the OS signal watcher does to the same channel in
    /// production.
    CancelledAfterDispatch {
        /// Attempt reference recorded before the answer was abandoned.
        attempt: String,
    },
    /// A long turn that notices the stop at a checkpoint before sending.
    CancelledBeforeSend,
    /// An orchestrator that hands back a reference which is not opaque.
    ///
    /// Built by decoding the raw value, so it bypasses the constructor exactly
    /// the way a record read back from disk or a carelessly built type would.
    SmuggledHandle {
        /// The raw value to hand back as an attempt reference.
        raw: String,
    },
}

impl FakeTurn {
    /// A turn that reached nothing outside the host.
    pub fn local(outcome: EngineOutcome) -> Self {
        Self::Receipt(TurnReceipt::local(outcome))
    }

    /// A turn that dispatched and reports its references.
    pub fn dispatched(
        outcome: EngineOutcome,
        disposition: DispatchDisposition,
        attempt: Option<&str>,
        receipt: Option<&str>,
    ) -> Self {
        Self::Receipt(TurnReceipt::dispatched(
            outcome,
            disposition,
            attempt.and_then(ExternalRef::new),
            receipt.and_then(ExternalRef::new),
        ))
    }

    /// A turn whose answer was lost after it went out.
    pub fn lost(attempt: &str) -> Self {
        Self::LostAfterDispatch {
            attempt: attempt.to_owned(),
        }
    }

    /// A long turn interrupted by a stop after the request went out.
    pub fn cancelled_after_dispatch(attempt: &str) -> Self {
        Self::CancelledAfterDispatch {
            attempt: attempt.to_owned(),
        }
    }

    /// A turn handing back a reference that is not a bounded opaque handle.
    pub fn smuggled_handle(raw: &str) -> Self {
        Self::SmuggledHandle {
            raw: raw.to_owned(),
        }
    }
}

/// A deterministic, offline orchestrator driven by a scripted turn list.
///
/// It reaches nothing: no provider, no network, no credential. Its only job is
/// to return exactly what the script says and record what it was asked to do.
/// Once the script is exhausted the last turn repeats, so a test can tick past
/// the end without the behaviour changing under it.
#[derive(Debug)]
pub struct FakeOrchestrator {
    binding: OrchestratorBinding,
    log: DispatchLog,
    turns: VecDeque<FakeTurn>,
    last: Option<FakeTurn>,
}

impl FakeOrchestrator {
    /// Build an orchestrator bound to one session and workspace.
    pub fn new(binding: OrchestratorBinding, log: DispatchLog, turns: Vec<FakeTurn>) -> Self {
        Self {
            binding,
            log,
            turns: turns.into(),
            last: None,
        }
    }

    /// An orchestrator bound to the standard fixture session and workspace.
    pub fn fixture(log: DispatchLog, turns: Vec<FakeTurn>) -> Self {
        Self::new(
            OrchestratorBinding::new("session-fixture", "project")
                .expect("fixture binding is bounded"),
            log,
            turns,
        )
    }
}

impl TurnOrchestrator for FakeOrchestrator {
    fn label(&self) -> &'static str {
        "fake"
    }

    fn binding(&self) -> OrchestratorBinding {
        self.binding.clone()
    }

    fn run_turn(&mut self, request: &TurnRequest<'_>) -> Result<TurnReceipt, TurnRefusal> {
        self.log.push(DispatchLogEntry {
            run_id: request.scope.run_id.clone(),
            ordinal: request.dispatch_ordinal,
            round: request.round,
            cancelled: request.cancel.is_cancelled(),
        });

        let turn = self.turns.pop_front().or_else(|| self.last.clone());
        let turn = match turn {
            Some(turn) => turn,
            None => {
                return Err(TurnRefusal::NotConfigured {
                    reason_code: "fake_unscripted".to_owned(),
                    detail: "no scripted turn".to_owned(),
                });
            }
        };
        self.last = Some(turn.clone());

        match turn {
            FakeTurn::Receipt(receipt) => Ok(receipt),
            FakeTurn::Refusal(refusal) => Err(refusal),
            FakeTurn::LostAfterDispatch { attempt } => Ok(TurnReceipt::dispatched(
                EngineOutcome::Failed {
                    reason_code: "answer_lost".to_owned(),
                    detail: "the connection dropped after the request went out".to_owned(),
                },
                DispatchDisposition::Indeterminate,
                ExternalRef::new(&attempt),
                None,
            )),
            FakeTurn::CancelledAfterDispatch { attempt } => {
                // The stop lands while the request is already out. Whether it
                // ran is exactly what this orchestrator cannot say.
                request.cancel.cancel();
                Ok(TurnReceipt::dispatched(
                    EngineOutcome::Failed {
                        reason_code: "cancelled_in_flight".to_owned(),
                        detail: "the host stopped while the request was outstanding".to_owned(),
                    },
                    DispatchDisposition::Indeterminate,
                    ExternalRef::new(&attempt),
                    None,
                ))
            }
            FakeTurn::CancelledBeforeSend => {
                request.cancel.cancel();
                Ok(TurnReceipt::dispatched(
                    EngineOutcome::NeedsAttention {
                        attention: crate::attention::AttentionKind::RecoveryRequired,
                        reason_code: "cancelled_before_send".to_owned(),
                        detail: "the host stopped before the request went out".to_owned(),
                    },
                    DispatchDisposition::NotDispatched,
                    None,
                    None,
                ))
            }
            FakeTurn::SmuggledHandle { raw } => Ok(TurnReceipt::dispatched(
                EngineOutcome::Progress {
                    update: serde_json::json!({ "note": "working" }),
                },
                DispatchDisposition::Resolved,
                serde_json::from_value(serde_json::Value::String(raw)).ok(),
                None,
            )),
        }
    }
}

/// A fixture script exercising progress, completion, escalation, and failure.
pub const FIXTURE_SCRIPT: &str = r#"{
  "prompts": {
    "build": [
      {"kind": "progress", "update": {"note": "planning"}},
      {"kind": "completed",
       "changedFiles": [{"path": "src/lib.rs", "summary": "add guard"}],
       "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n",
       "fingerprint": "fingerprint-build"}
    ],
    "escalate": [
      {"kind": "needsAttention", "attention": "permission_required",
       "reasonCode": "shell_write_requested", "detail": "engine asked to write outside the run"}
    ],
    "fail": [
      {"kind": "failed", "reasonCode": "engine_refused", "detail": "no route"}
    ],
    "forever": [
      {"kind": "progress", "update": {"note": "still working"}}
    ],
    "noop": [
      {"kind": "completed", "changedFiles": [], "diff": "", "fingerprint": "fingerprint-noop"}
    ],
    "leak": [
      {"kind": "failed", "reasonCode": "engine_leak",
       "detail": "retry with XAI_API_KEY=xai-abcdefghijklmnopqrstuvwxyz012345"}
    ],
    "escape": [
      {"kind": "completed", "changedFiles": [{"path": "/etc/shadow", "summary": "x"}],
       "diff": "", "fingerprint": "fingerprint-escape"}
    ]
  },
  "default": [
    {"kind": "failed", "reasonCode": "unscripted", "detail": "no scripted outcome"}
  ]
}"#;
