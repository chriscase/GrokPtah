//! Sep 18 native host-sentinel runner — live Mac collector path.
//!
//! Wires [`MacHostSentinelCollector`] into one explicitly labeled runner so the
//! baseline and every lifecycle/Stop probe come from the native collector.
//! Never falls back to [`SyntheticHostProbe`] on TCC, timeout, or failure.
//! Live collection is **not** VF/isolation/physical PASS and never enables
//! admission or Computer Mode.
//!
//! Linux CI / non-macOS: honest `UnsupportedPlatform` artifact. Guest remains
//! the synthetic backend; only host-sentinel provenance is native on macOS.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::HarnessResult;
use crate::lifecycle::ProofEvidenceClass;
use crate::proof_sequencer::SealedProofEvidence;
use crate::NATIVE_HOST_SENTINEL_NONCLAIM;

#[cfg(target_os = "macos")]
use crate::error::{HarnessError, HarnessErrorCode};
#[cfg(target_os = "macos")]
use crate::harness::IsolatedSurfaceHarness;
#[cfg(target_os = "macos")]
use crate::proof_sequencer::ChecklistStep;
#[cfg(target_os = "macos")]
use crate::simulator::GuestLocalAction;

/// Where the native host-sentinel runner executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSentinelRunnerPlatform {
    /// Linux CI and other non-macOS hosts — collector does not exist.
    NonMacOs,
    /// macOS worker; collector exists and may succeed or fail closed.
    MacOs,
}

/// Outcome of the native host-sentinel runner. Never a VF/isolation PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeSentinelRunnerOutcome {
    /// Non-macOS host — native collection is unavailable.
    UnsupportedPlatform,
    /// macOS collector failed closed (TCC, Accessibility, or host read).
    BackendUnavailable,
    /// macOS collector supplied baseline + exclusive lifecycle probes.
    NativeProbesCompleted,
}

/// Honest artifact from [`run_native_host_sentinel`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeSentinelEvidence {
    pub platform: NativeSentinelRunnerPlatform,
    pub outcome: NativeSentinelRunnerOutcome,
    pub evidence_class: ProofEvidenceClass,
    pub checklist_completed: bool,
    pub live_host_sentinel_collection: bool,
    pub independent_collection_verified: bool,
    pub synthetic_fallback_used: bool,
    pub stop_fence_applied: bool,
    pub channels_destroyed: usize,
    pub post_stop_inject_fenced: bool,
    pub physical_pass_claimed: bool,
    pub isolation_pass_claimed: bool,
    pub vf_pass_claimed: bool,
    pub collector_error: Option<String>,
    pub sealed_evidence: Option<SealedProofEvidence>,
    pub nonclaim: String,
    pub recorded_at: DateTime<Utc>,
}

impl NativeSentinelEvidence {
    fn honest_shell(
        platform: NativeSentinelRunnerPlatform,
        outcome: NativeSentinelRunnerOutcome,
    ) -> Self {
        Self {
            platform,
            outcome,
            evidence_class: ProofEvidenceClass::Synthetic,
            checklist_completed: false,
            live_host_sentinel_collection: false,
            independent_collection_verified: false,
            synthetic_fallback_used: false,
            stop_fence_applied: false,
            channels_destroyed: 0,
            post_stop_inject_fenced: false,
            physical_pass_claimed: false,
            isolation_pass_claimed: false,
            vf_pass_claimed: false,
            collector_error: None,
            sealed_evidence: None,
            nonclaim: NATIVE_HOST_SENTINEL_NONCLAIM.into(),
            recorded_at: Utc::now(),
        }
    }

    pub fn unsupported_non_macos() -> Self {
        Self::honest_shell(
            NativeSentinelRunnerPlatform::NonMacOs,
            NativeSentinelRunnerOutcome::UnsupportedPlatform,
        )
    }
}

/// Run the native host-sentinel runner.
///
/// On non-macOS this never constructs a synthetic probe labeled as native, never
/// runs the guest checklist as a stand-in, and never claims live collection.
pub fn run_native_host_sentinel(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    run_native_host_sentinel_impl(checkout_path, snapshot_root)
}

#[cfg(not(target_os = "macos"))]
fn run_native_host_sentinel_impl(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    let _ = (checkout_path, snapshot_root);
    Ok(NativeSentinelEvidence::unsupported_non_macos())
}

#[cfg(target_os = "macos")]
fn run_native_host_sentinel_impl(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    use crate::sentinel::MacHostSentinelCollector;

    let collector = MacHostSentinelCollector::new(checkout_path.display().to_string());
    let baseline = match collector.collect() {
        Ok(baseline) => baseline,
        Err(err) => {
            let mut evidence = NativeSentinelEvidence::honest_shell(
                NativeSentinelRunnerPlatform::MacOs,
                NativeSentinelRunnerOutcome::BackendUnavailable,
            );
            evidence.collector_error = Some(err.message);
            return Ok(evidence);
        }
    };

    let mut harness = IsolatedSurfaceHarness::new(baseline);
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }
    harness.attach_native_collector(collector.clone())?;

    match run_native_checklist(&mut harness, &collector) {
        Ok(evidence) => Ok(evidence),
        Err(err) if err.code == HarnessErrorCode::BackendUnavailable => {
            let mut evidence = NativeSentinelEvidence::honest_shell(
                NativeSentinelRunnerPlatform::MacOs,
                NativeSentinelRunnerOutcome::BackendUnavailable,
            );
            evidence.collector_error = Some(err.message);
            let _ = harness.stop();
            Ok(evidence)
        }
        Err(err) => Err(err),
    }
}

#[cfg(target_os = "macos")]
fn run_native_checklist(
    harness: &mut IsolatedSurfaceHarness,
    collector: &crate::sentinel::MacHostSentinelCollector,
) -> HarnessResult<NativeSentinelEvidence> {
    let declared_class = harness.evidence_class();
    crate::backend::assert_evidence_class_unchanged(declared_class, ProofEvidenceClass::Synthetic)?;

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

    let independent = collector.collect().map_err(|err| {
        HarnessError::backend_unavailable(format!(
            "independent native collection failed closed: {}",
            err.message
        ))
    })?;
    if &independent != harness.sentinels().baseline() {
        return Err(HarnessError::host_sentinel_violation(
            "independent native collection drifted from collector baseline",
        ));
    }

    let stop_evidence = harness.stop()?;
    let stop_fence_applied = stop_evidence.backend_fence_error.is_none();
    if stop_evidence.destroy_confirmed(harness.lifecycle().phase) {
        steps.push(ChecklistStep::StopDestroyed);
    }

    let inject_err = harness
        .inject_guest_action(GuestLocalAction::ClickGuestButton)
        .expect_err("stale inject token must be rejected");
    if inject_err.code != HarnessErrorCode::InjectFenced {
        return Err(HarnessError::invalid_state(
            "stale token rejection must fence inject",
        ));
    }
    steps.push(ChecklistStep::StaleTokensRejected);

    crate::backend::assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
    steps.push(ChecklistStep::EvidenceSealed);

    if !stop_evidence.live_host_sentinel_collection {
        return Err(HarnessError::invalid_state(
            "native host-sentinel runner Stop must retain live native provenance (synthetic fallback is forbidden)",
        ));
    }

    let sealed = SealedProofEvidence {
        evidence_class: declared_class,
        stop_evidence: stop_evidence.clone(),
        checklist_steps: steps,
        fault_matrix_case: None,
        sealed_at: Utc::now(),
        nonclaim: NATIVE_HOST_SENTINEL_NONCLAIM.into(),
    };

    Ok(NativeSentinelEvidence {
        platform: NativeSentinelRunnerPlatform::MacOs,
        outcome: NativeSentinelRunnerOutcome::NativeProbesCompleted,
        evidence_class: declared_class,
        checklist_completed: true,
        live_host_sentinel_collection: stop_evidence.live_host_sentinel_collection,
        independent_collection_verified: true,
        synthetic_fallback_used: false,
        stop_fence_applied,
        channels_destroyed: stop_evidence.channels_destroyed,
        post_stop_inject_fenced: true,
        physical_pass_claimed: false,
        isolation_pass_claimed: false,
        vf_pass_claimed: false,
        collector_error: stop_evidence
            .host_sentinel_probe_error
            .as_ref()
            .map(|err| err.message.clone()),
        sealed_evidence: Some(sealed),
        nonclaim: NATIVE_HOST_SENTINEL_NONCLAIM.into(),
        recorded_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_native_runner_is_unsupported_without_synthetic_fallback() {
        let evidence = run_native_host_sentinel(Path::new("."), None).expect("artifact");
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(evidence.platform, NativeSentinelRunnerPlatform::NonMacOs);
            assert_eq!(
                evidence.outcome,
                NativeSentinelRunnerOutcome::UnsupportedPlatform
            );
            assert!(!evidence.live_host_sentinel_collection);
            assert!(!evidence.independent_collection_verified);
            assert!(!evidence.synthetic_fallback_used);
            assert!(!evidence.checklist_completed);
            assert!(!evidence.physical_pass_claimed);
            assert!(!evidence.isolation_pass_claimed);
            assert!(!evidence.vf_pass_claimed);
            assert!(evidence.sealed_evidence.is_none());
            assert_eq!(evidence.nonclaim, NATIVE_HOST_SENTINEL_NONCLAIM);
            assert!(!crate::isolated_surface_admission_available());
        }
        #[cfg(target_os = "macos")]
        {
            assert_eq!(evidence.platform, NativeSentinelRunnerPlatform::MacOs);
            assert!(!evidence.physical_pass_claimed);
            assert!(!evidence.isolation_pass_claimed);
            assert!(!evidence.vf_pass_claimed);
            assert!(!evidence.synthetic_fallback_used);
            assert!(!crate::isolated_surface_admission_available());
            match evidence.outcome {
                NativeSentinelRunnerOutcome::BackendUnavailable => {
                    assert!(!evidence.live_host_sentinel_collection);
                    assert!(evidence.collector_error.is_some());
                }
                NativeSentinelRunnerOutcome::NativeProbesCompleted => {
                    assert!(evidence.live_host_sentinel_collection);
                    assert!(evidence.independent_collection_verified);
                    assert!(evidence.post_stop_inject_fenced);
                    assert!(evidence.channels_destroyed > 0);
                }
                NativeSentinelRunnerOutcome::UnsupportedPlatform => {
                    panic!("macOS native runner must not report NonMacOs unsupported");
                }
            }
        }
    }
}
