//! Sep 18 no-model proof sequencer — synthetic backend checklist + fault matrix.
//!
//! Runs the bounded Windowed Coding Run v0 proof without a live model:
//! arm → boot → frame+challenge → one guest-local action (possible before
//! boundary) → postcondition → Stop/destroy → reject stale tokens → seal.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::assert_evidence_class_unchanged;
use crate::error::{HarnessError, HarnessErrorCode, HarnessResult};
use crate::harness::{IsolatedSurfaceHarness, StopEvidence};
use crate::lifecycle::{GuestLifecycleDisposition, GuestLifecyclePhase, ProofEvidenceClass};
use crate::sentinel::HostSentinelSnapshot;
use crate::simulator::{GuestLocalAction, SyntheticGuest};
use crate::SYNTHETIC_HARNESS_NONCLAIM;

/// Bounded fault-matrix cases exercised against the synthetic backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultMatrixCase {
    BootStop,
    PreDispatchStop,
    LostAckUncertain,
    RestartNoReplay,
}

/// Checklist step recorded in sealed evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecklistStep {
    Armed,
    Booted,
    FrameChallenge,
    GuestLocalActionMarkedPossible,
    PostconditionVerified,
    StopDestroyed,
    StaleTokensRejected,
    EvidenceSealed,
}

/// Sealed proof artifact from the no-model sequencer. Labels are fixed at seal
/// time and must never be upgraded to `VirtualizationFramework` from Linux CI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SealedProofEvidence {
    pub evidence_class: ProofEvidenceClass,
    pub stop_evidence: StopEvidence,
    pub checklist_steps: Vec<ChecklistStep>,
    pub fault_matrix_case: Option<FaultMatrixCase>,
    pub sealed_at: DateTime<Utc>,
    pub nonclaim: String,
}

/// Sep 18 no-model proof sequencer. Drives the harness against the synthetic
/// backend (or any `IsolatedSurfaceBackend`) without live provider calls.
pub struct Sep18NoModelProofSequencer {
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<std::path::PathBuf>,
}

impl Sep18NoModelProofSequencer {
    pub fn new(baseline: HostSentinelSnapshot) -> Self {
        Self {
            baseline,
            snapshot_root: None,
        }
    }

    pub fn with_snapshot_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.snapshot_root = Some(root.into());
        self
    }

    /// Happy-path checklist against the default synthetic backend.
    pub fn run_happy_path(&self) -> HarnessResult<SealedProofEvidence> {
        let mut harness = self.harness();
        self.run_checklist(&mut harness, None)
    }

    /// Run one bounded fault-matrix case.
    pub fn run_fault_matrix(&self, case: FaultMatrixCase) -> HarnessResult<SealedProofEvidence> {
        match case {
            FaultMatrixCase::BootStop => self.run_boot_stop(),
            FaultMatrixCase::PreDispatchStop => self.run_pre_dispatch_stop(),
            FaultMatrixCase::LostAckUncertain => self.run_lost_ack_uncertain(),
            FaultMatrixCase::RestartNoReplay => self.run_restart_no_replay(),
        }
    }

    fn harness(&self) -> IsolatedSurfaceHarness<SyntheticGuest> {
        let mut harness = IsolatedSurfaceHarness::new(self.baseline.clone());
        if let Some(root) = &self.snapshot_root {
            harness = harness.with_snapshot_root(root);
        }
        harness
    }

    fn run_checklist(
        &self,
        harness: &mut IsolatedSurfaceHarness<SyntheticGuest>,
        fault_case: Option<FaultMatrixCase>,
    ) -> HarnessResult<SealedProofEvidence> {
        let declared_class = harness.evidence_class();
        assert_evidence_class_unchanged(declared_class, ProofEvidenceClass::Synthetic)?;

        let mut steps = Vec::new();
        steps.push(ChecklistStep::Armed);

        harness.boot()?;
        steps.push(ChecklistStep::Booted);

        let before = harness.observe_frame()?;
        if before.epoch == 0 {
            return Err(HarnessError::invalid_state(
                "frame challenge requires booted epoch",
            ));
        }
        steps.push(ChecklistStep::FrameChallenge);

        let delta = harness.inject_guest_action(GuestLocalAction::ClickGuestButton)?;
        steps.push(ChecklistStep::GuestLocalActionMarkedPossible);

        let after = harness.observe_frame()?;
        if !delta.guest_local_change || before.digest == after.digest {
            return Err(HarnessError::invalid_state(
                "postcondition requires guest-local frame change",
            ));
        }
        steps.push(ChecklistStep::PostconditionVerified);

        let stop_evidence = harness.stop()?;
        steps.push(ChecklistStep::StopDestroyed);

        let inject_err = harness
            .inject_guest_action(GuestLocalAction::ClickGuestButton)
            .expect_err("stale inject token must be rejected");
        if inject_err.code != HarnessErrorCode::InjectFenced {
            return Err(HarnessError::invalid_state(
                "stale token rejection must fence inject",
            ));
        }
        steps.push(ChecklistStep::StaleTokensRejected);

        assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
        steps.push(ChecklistStep::EvidenceSealed);

        Ok(SealedProofEvidence {
            evidence_class: declared_class,
            stop_evidence,
            checklist_steps: steps,
            fault_matrix_case: fault_case,
            sealed_at: Utc::now(),
            nonclaim: SYNTHETIC_HARNESS_NONCLAIM.into(),
        })
    }

    fn run_boot_stop(&self) -> HarnessResult<SealedProofEvidence> {
        let mut harness = self.harness();
        let declared_class = harness.evidence_class();
        harness.boot()?;
        let stop_evidence = harness.stop()?;
        assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
        Ok(SealedProofEvidence {
            evidence_class: declared_class,
            stop_evidence,
            checklist_steps: vec![
                ChecklistStep::Armed,
                ChecklistStep::Booted,
                ChecklistStep::StopDestroyed,
                ChecklistStep::EvidenceSealed,
            ],
            fault_matrix_case: Some(FaultMatrixCase::BootStop),
            sealed_at: Utc::now(),
            nonclaim: SYNTHETIC_HARNESS_NONCLAIM.into(),
        })
    }

    fn run_pre_dispatch_stop(&self) -> HarnessResult<SealedProofEvidence> {
        let mut harness = self.harness();
        let declared_class = harness.evidence_class();
        harness.boot()?;
        harness.observe_frame()?;
        // Stop before any guest-local inject dispatch.
        let stop_evidence = harness.stop()?;
        assert_eq!(
            stop_evidence.disposition,
            Some(GuestLifecycleDisposition::Stopped)
        );
        assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
        Ok(SealedProofEvidence {
            evidence_class: declared_class,
            stop_evidence,
            checklist_steps: vec![
                ChecklistStep::Armed,
                ChecklistStep::Booted,
                ChecklistStep::FrameChallenge,
                ChecklistStep::StopDestroyed,
                ChecklistStep::EvidenceSealed,
            ],
            fault_matrix_case: Some(FaultMatrixCase::PreDispatchStop),
            sealed_at: Utc::now(),
            nonclaim: SYNTHETIC_HARNESS_NONCLAIM.into(),
        })
    }

    fn run_lost_ack_uncertain(&self) -> HarnessResult<SealedProofEvidence> {
        let mut harness = self.harness();
        let declared_class = harness.evidence_class();
        harness.boot()?;
        harness.schedule_uncertain_on_next_inject();
        let err = harness
            .inject_guest_action(GuestLocalAction::ClickGuestButton)
            .expect_err("lost ack");
        assert_eq!(err.code, HarnessErrorCode::UncertainOutcome);

        let stop_evidence = harness.stop()?;
        assert_eq!(
            stop_evidence.disposition,
            Some(GuestLifecycleDisposition::Uncertain)
        );
        assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
        Ok(SealedProofEvidence {
            evidence_class: declared_class,
            stop_evidence,
            checklist_steps: vec![
                ChecklistStep::Armed,
                ChecklistStep::Booted,
                ChecklistStep::GuestLocalActionMarkedPossible,
                ChecklistStep::StopDestroyed,
                ChecklistStep::EvidenceSealed,
            ],
            fault_matrix_case: Some(FaultMatrixCase::LostAckUncertain),
            sealed_at: Utc::now(),
            nonclaim: SYNTHETIC_HARNESS_NONCLAIM.into(),
        })
    }

    fn run_restart_no_replay(&self) -> HarnessResult<SealedProofEvidence> {
        let root = self.snapshot_root.as_ref().ok_or_else(|| {
            HarnessError::invalid_state("snapshot root required for restart case")
        })?;
        let mut harness = self.harness();
        harness.boot()?;
        harness.schedule_uncertain_on_next_inject();
        harness
            .inject_guest_action(GuestLocalAction::ClickGuestButton)
            .expect_err("uncertain");

        let mut restarted =
            IsolatedSurfaceHarness::new(self.baseline.clone()).with_snapshot_root(root);
        let stop_evidence = restarted.recover_after_restart()?;
        assert_eq!(restarted.lifecycle().phase, GuestLifecyclePhase::Destroyed);
        assert_eq!(
            restarted.lifecycle().disposition,
            Some(GuestLifecycleDisposition::Uncertain)
        );
        assert!(
            stop_evidence.channels_destroyed > 0 || restarted.channels().open_count() == 0,
            "restart recovery must tear down channels from snapshot"
        );

        let retry_err = restarted
            .retry_inject_after_uncertain(GuestLocalAction::ClickGuestButton)
            .expect_err("no replay");
        assert_eq!(retry_err.code, HarnessErrorCode::AutoRetryForbidden);

        let declared_class = restarted.evidence_class();
        assert_evidence_class_unchanged(declared_class, ProofEvidenceClass::Synthetic)?;

        Ok(SealedProofEvidence {
            evidence_class: declared_class,
            stop_evidence,
            checklist_steps: vec![
                ChecklistStep::Armed,
                ChecklistStep::Booted,
                ChecklistStep::GuestLocalActionMarkedPossible,
                ChecklistStep::StopDestroyed,
                ChecklistStep::StaleTokensRejected,
                ChecklistStep::EvidenceSealed,
            ],
            fault_matrix_case: Some(FaultMatrixCase::RestartNoReplay),
            sealed_at: Utc::now(),
            nonclaim: SYNTHETIC_HARNESS_NONCLAIM.into(),
        })
    }
}
