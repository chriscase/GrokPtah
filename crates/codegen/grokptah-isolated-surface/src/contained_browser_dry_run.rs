//! Platform-neutral Contained Browser dry-run for the Sep 18 sequencer pivot.
//!
//! Exercises the Contained Browser substrate checklist and bounded fault cuts
//! without claiming Virtualization.framework PASS or Sep 18 isolation PASS.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::assert_evidence_class_unchanged;
use crate::contained_browser::ContainedBrowserBackend;
use crate::error::{HarnessError, HarnessErrorCode, HarnessResult};
use crate::harness::IsolatedSurfaceHarness;
use crate::lifecycle::{GuestLifecycleDisposition, ProofEvidenceClass};
use crate::proof_sequencer::{ChecklistStep, FaultMatrixCase, SealedProofEvidence};
use crate::sentinel::HostSentinelSnapshot;
use crate::simulator::{FaultInjectingBackend, GuestLocalAction};
use crate::CONTAINED_BROWSER_DRY_RUN_NONCLAIM;

/// Where the Contained Browser dry-run ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainedBrowserDryRunPlatform {
    /// Default in-process browser simulator (Linux CI and macOS without engine).
    SimulatorSubstrate,
    /// `browser-engine` feature enabled — substrate boot fails closed.
    BrowserEngineFeatureDisabled,
}

/// Outcome of a Contained Browser dry-run. Never represents Sep 18 isolation PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainedBrowserDryRunOutcome {
    /// Checklist completed on simulator substrate; isolation not proven.
    SubstrateRehearsal,
    /// Optional engine feature path — boot fails closed.
    BackendUnavailable,
}

/// Honest artifact from [`run_contained_browser_dry_run`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainedBrowserDryRunEvidence {
    pub platform: ContainedBrowserDryRunPlatform,
    pub outcome: ContainedBrowserDryRunOutcome,
    pub evidence_class: ProofEvidenceClass,
    pub checklist_completed: bool,
    pub isolation_pass_claimed: bool,
    pub vf_pass_claimed: bool,
    pub nonclaim: String,
    pub sealed_evidence: Option<SealedProofEvidence>,
    pub recorded_at: DateTime<Utc>,
}

/// Run the bounded Contained Browser dry-run happy-path checklist.
pub fn run_contained_browser_dry_run(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<ContainedBrowserDryRunEvidence> {
    #[cfg(feature = "browser-engine")]
    {
        return run_contained_browser_engine_unavailable(baseline);
    }

    #[cfg(not(feature = "browser-engine"))]
    {
        run_contained_browser_simulator_checklist(baseline, snapshot_root)
    }
}

/// Run one bounded fault-matrix case against the Contained Browser substrate.
pub fn run_contained_browser_fault_matrix(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
    case: FaultMatrixCase,
) -> HarnessResult<SealedProofEvidence> {
    #[cfg(feature = "browser-engine")]
    {
        let _ = (baseline, snapshot_root, case);
        return Err(HarnessError::backend_unavailable(
            "Contained Browser fault matrix requires simulator substrate; browser-engine fails closed",
        ));
    }

    #[cfg(not(feature = "browser-engine"))]
    {
        match case {
            FaultMatrixCase::BootStop => run_contained_browser_boot_stop(baseline, snapshot_root),
            FaultMatrixCase::PreDispatchStop => {
                run_contained_browser_pre_dispatch_stop(baseline, snapshot_root)
            }
            FaultMatrixCase::LostAckUncertain => {
                run_contained_browser_lost_ack_uncertain(baseline, snapshot_root)
            }
            FaultMatrixCase::RestartNoReplay => {
                run_contained_browser_restart_no_replay(baseline, snapshot_root)
            }
        }
    }
}

#[cfg(feature = "browser-engine")]
fn run_contained_browser_engine_unavailable(
    baseline: HostSentinelSnapshot,
) -> HarnessResult<ContainedBrowserDryRunEvidence> {
    let mut harness =
        IsolatedSurfaceHarness::with_backend(baseline, ContainedBrowserBackend::new())?;
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::ContainedBrowser
    );

    let boot_err = harness
        .boot()
        .expect_err("browser-engine substrate boot must fail closed");
    if boot_err.code != HarnessErrorCode::BackendUnavailable {
        return Err(HarnessError::invalid_state(format!(
            "browser-engine boot must return BackendUnavailable, got {:?}",
            boot_err.code
        )));
    }

    let stop_evidence = harness.stop()?;
    if stop_evidence.disposition.is_none() {
        return Err(HarnessError::invalid_state(
            "browser-engine dry-run stop must record a disposition",
        ));
    }

    Ok(ContainedBrowserDryRunEvidence {
        platform: ContainedBrowserDryRunPlatform::BrowserEngineFeatureDisabled,
        outcome: ContainedBrowserDryRunOutcome::BackendUnavailable,
        evidence_class: ProofEvidenceClass::ContainedBrowser,
        checklist_completed: false,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
        sealed_evidence: None,
        recorded_at: Utc::now(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_simulator_checklist(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<ContainedBrowserDryRunEvidence> {
    let backend = ContainedBrowserBackend::new();
    assert!(!backend.isolation_proof_available());

    let mut harness = IsolatedSurfaceHarness::with_backend(baseline, backend)?;
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }

    let sealed = run_contained_browser_checklist(&mut harness, None)?;

    Ok(ContainedBrowserDryRunEvidence {
        platform: ContainedBrowserDryRunPlatform::SimulatorSubstrate,
        outcome: ContainedBrowserDryRunOutcome::SubstrateRehearsal,
        evidence_class: ProofEvidenceClass::ContainedBrowser,
        checklist_completed: true,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
        sealed_evidence: Some(sealed),
        recorded_at: Utc::now(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_checklist(
    harness: &mut IsolatedSurfaceHarness<ContainedBrowserBackend>,
    fault_case: Option<FaultMatrixCase>,
) -> HarnessResult<SealedProofEvidence> {
    let declared_class = harness.evidence_class();
    assert_evidence_class_unchanged(declared_class, ProofEvidenceClass::ContainedBrowser)?;

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
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_boot_stop(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<SealedProofEvidence> {
    let mut harness =
        IsolatedSurfaceHarness::with_backend(baseline, ContainedBrowserBackend::new())?;
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }
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
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_pre_dispatch_stop(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<SealedProofEvidence> {
    let mut harness =
        IsolatedSurfaceHarness::with_backend(baseline, ContainedBrowserBackend::new())?;
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }
    let declared_class = harness.evidence_class();
    harness.boot()?;
    harness.observe_frame()?;
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
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_lost_ack_uncertain(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<SealedProofEvidence> {
    let mut backend = ContainedBrowserBackend::new();
    backend.schedule_uncertain_on_next_inject();
    let mut harness = IsolatedSurfaceHarness::with_backend(baseline, backend)?;
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }
    let declared_class = harness.evidence_class();
    harness.boot()?;
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
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
    })
}

#[cfg(not(feature = "browser-engine"))]
fn run_contained_browser_restart_no_replay(
    baseline: HostSentinelSnapshot,
    snapshot_root: Option<&std::path::Path>,
) -> HarnessResult<SealedProofEvidence> {
    let root = snapshot_root
        .ok_or_else(|| HarnessError::invalid_state("snapshot root required for restart case"))?;
    let mut backend = ContainedBrowserBackend::new();
    backend.schedule_uncertain_on_next_inject();
    let mut harness = IsolatedSurfaceHarness::with_backend(baseline.clone(), backend)?;
    harness = harness.with_snapshot_root(root);
    harness.boot()?;
    harness
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect_err("uncertain");

    let mut restarted =
        IsolatedSurfaceHarness::with_backend(baseline, ContainedBrowserBackend::new())?
            .with_snapshot_root(root);
    let stop_evidence = restarted.recover_after_restart()?;
    assert_eq!(
        restarted.lifecycle().phase,
        crate::lifecycle::GuestLifecyclePhase::Destroyed
    );
    assert_eq!(
        restarted.lifecycle().disposition,
        Some(GuestLifecycleDisposition::Uncertain)
    );

    let retry_err = restarted
        .retry_inject_after_uncertain(GuestLocalAction::ClickGuestButton)
        .expect_err("no replay");
    assert_eq!(retry_err.code, HarnessErrorCode::AutoRetryForbidden);

    let declared_class = restarted.evidence_class();
    assert_evidence_class_unchanged(declared_class, ProofEvidenceClass::ContainedBrowser)?;

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
        nonclaim: CONTAINED_BROWSER_DRY_RUN_NONCLAIM.into(),
    })
}

#[cfg(not(feature = "browser-engine"))]
pub fn run_contained_browser_stop_fence_regression(
    baseline: HostSentinelSnapshot,
) -> HarnessResult<()> {
    let mut wrapped = FaultInjectingBackend::new(ContainedBrowserBackend::new());
    wrapped.fail_stop_fence = true;

    let mut harness = IsolatedSurfaceHarness::with_backend(baseline, wrapped)?;
    harness.boot()?;
    assert_eq!(harness.channels().open_count(), 2);

    let evidence = harness.stop()?;
    assert!(evidence.backend_fence_error.is_some());
    assert_eq!(evidence.channels_destroyed, 2);
    assert_eq!(
        harness.lifecycle().phase,
        crate::lifecycle::GuestLifecyclePhase::Destroyed
    );
    harness.channels().assert_all_destroyed()?;
    Ok(())
}
