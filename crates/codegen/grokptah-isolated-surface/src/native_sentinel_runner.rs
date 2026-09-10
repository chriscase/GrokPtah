//! Sep 18 native host-sentinel runner — live Mac collector path.
//!
//! Wires [`MacHostSentinelCollector`] into one explicitly labeled runner so the
//! baseline and every lifecycle/Stop probe come from the native collector.
//! Native mode also requires VF dry-run: the collector is attached **before**
//! VF boot/lifecycle/Stop. Never falls back to [`SyntheticHostProbe`] on TCC,
//! timeout, or failure. Live collection is **not** VF/isolation/physical PASS
//! and never enables admission or Computer Mode.
//!
//! Linux CI / non-macOS: honest `UnsupportedPlatform` artifact. Guest remains
//! the synthetic backend; only host-sentinel provenance is native on macOS.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::VfLaunchReceipt;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::proof_sequencer::SealedProofEvidence;
use crate::sentinel::HostSentinelProbeKind;
use crate::vf_dry_run::{
    run_vf_dry_run_with_native_host_sentinels, VfDryRunEvidence, VfDryRunOutcome, VfDryRunPlatform,
};
use crate::NATIVE_HOST_SENTINEL_NONCLAIM;

#[cfg(target_os = "macos")]
use crate::error::HarnessErrorCode;
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
    #[serde(default)]
    pub host_sentinel_probes_performed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_host_sentinel_probe_kind: Option<HostSentinelProbeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vf_dry_run: Option<VfDryRunEvidence>,
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
            host_sentinel_probes_performed: 0,
            last_host_sentinel_probe_kind: None,
            vf_dry_run: None,
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

    /// macOS collector/runtime failure — never labeled NonMacOs/UnsupportedPlatform.
    pub fn macos_fail_closed(
        outcome: NativeSentinelRunnerOutcome,
        message: impl Into<String>,
    ) -> Self {
        let mut evidence = Self::honest_shell(NativeSentinelRunnerPlatform::MacOs, outcome);
        evidence.collector_error = Some(message.into());
        evidence.synthetic_fallback_used = false;
        evidence
    }

    pub fn fail_closed_for_current_platform(err: &HarnessError) -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::macos_fail_closed(
                NativeSentinelRunnerOutcome::BackendUnavailable,
                err.message.clone(),
            )
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = err;
            Self::unsupported_non_macos()
        }
    }
}

fn require_explicit_checkout(checkout_path: &Path) -> HarnessResult<()> {
    if checkout_path.as_os_str().is_empty() {
        return Err(HarnessError::invalid_state(
            "native host-sentinel runner requires an explicitly supplied checkout PATH",
        ));
    }
    Ok(())
}

fn native_vf_receipt() -> VfLaunchReceipt {
    VfLaunchReceipt {
        physical_mac_proof_id: "sep18-native-vf-dry-run".into(),
    }
}

/// Run the native host-sentinel runner.
///
/// Native mode requires an explicit checkout path (never defaulted to `.`) and
/// always rehearses VF dry-run with the collector attached before VF boot.
/// On non-macOS this never constructs a synthetic probe labeled as native, never
/// runs the guest checklist as a stand-in, and never claims live collection.
pub fn run_native_host_sentinel(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    require_explicit_checkout(checkout_path)?;
    run_native_host_sentinel_impl(checkout_path, snapshot_root)
}

fn attach_vf_dry_run(
    mut evidence: NativeSentinelEvidence,
    vf: VfDryRunEvidence,
) -> NativeSentinelEvidence {
    if evidence.live_host_sentinel_collection
        && vf.last_host_sentinel_probe_kind == Some(HostSentinelProbeKind::NativeMacHost)
        && evidence.last_host_sentinel_probe_kind.is_none()
    {
        evidence.last_host_sentinel_probe_kind = vf.last_host_sentinel_probe_kind;
        evidence.host_sentinel_probes_performed = vf.host_sentinel_probes_performed;
        evidence.channels_destroyed = vf.channels_destroyed;
    }
    evidence.vf_dry_run = Some(vf);
    evidence
}

#[cfg(not(target_os = "macos"))]
fn run_native_host_sentinel_impl(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    let _ = snapshot_root;
    let vf = run_vf_dry_run_with_native_host_sentinels(native_vf_receipt(), checkout_path)?;
    debug_assert_eq!(vf.platform, VfDryRunPlatform::NonMacOs);
    debug_assert_eq!(vf.outcome, VfDryRunOutcome::UnsupportedPlatform);
    debug_assert!(vf.native_host_sentinels_requested);
    Ok(attach_vf_dry_run(
        NativeSentinelEvidence::unsupported_non_macos(),
        vf,
    ))
}

#[cfg(target_os = "macos")]
fn run_native_host_sentinel_impl(
    checkout_path: &Path,
    snapshot_root: Option<&Path>,
) -> HarnessResult<NativeSentinelEvidence> {
    use crate::sentinel::MacHostSentinelCollector;

    let vf = run_vf_dry_run_with_native_host_sentinels(native_vf_receipt(), checkout_path)?;
    if vf.platform == VfDryRunPlatform::NonMacOs
        || vf.outcome == VfDryRunOutcome::UnsupportedPlatform
    {
        let mut evidence = NativeSentinelEvidence::macos_fail_closed(
            NativeSentinelRunnerOutcome::BackendUnavailable,
            "macOS native host-sentinel runner must not seal NonMacOs/UnsupportedPlatform",
        );
        evidence.vf_dry_run = Some(vf);
        return Ok(evidence);
    }

    let collector = MacHostSentinelCollector::new(checkout_path.display().to_string());
    let baseline = match collector.collect() {
        Ok(baseline) => baseline,
        Err(err) => {
            return Ok(attach_vf_dry_run(
                NativeSentinelEvidence::macos_fail_closed(
                    NativeSentinelRunnerOutcome::BackendUnavailable,
                    err.message,
                ),
                vf,
            ));
        }
    };

    let mut harness = IsolatedSurfaceHarness::new(baseline);
    if let Some(root) = snapshot_root {
        harness = harness.with_snapshot_root(root);
    }
    harness.attach_native_collector(collector.clone())?;

    match run_native_checklist(&mut harness, &collector) {
        Ok(evidence) => Ok(attach_vf_dry_run(evidence, vf)),
        Err(err) if err.code == HarnessErrorCode::BackendUnavailable => Ok(attach_vf_dry_run(
            NativeSentinelEvidence::macos_fail_closed(
                NativeSentinelRunnerOutcome::BackendUnavailable,
                err.message,
            ),
            vf,
        )),
        Err(err) => {
            let _ = harness.stop();
            Err(err)
        }
    }
}

#[cfg(target_os = "macos")]
fn require_post_stop_inject_fenced(
    result: HarnessResult<crate::simulator::FrameDelta>,
) -> HarnessResult<()> {
    match result {
        Err(err) if err.code == HarnessErrorCode::InjectFenced => Ok(()),
        Err(err) => Err(HarnessError::invalid_state(format!(
            "stale token rejection must fence inject, got {:?}",
            err.code
        ))),
        Ok(_) => Err(HarnessError::inject_fenced(
            "stale inject token must be rejected after Stop",
        )),
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

    require_post_stop_inject_fenced(
        harness.inject_guest_action(GuestLocalAction::ClickGuestButton),
    )?;
    steps.push(ChecklistStep::StaleTokensRejected);

    crate::backend::assert_evidence_class_unchanged(declared_class, harness.evidence_class())?;
    steps.push(ChecklistStep::EvidenceSealed);

    if !stop_evidence.live_host_sentinel_collection {
        return Err(HarnessError::invalid_state(
            "native host-sentinel runner Stop must retain live native provenance (synthetic fallback is forbidden)",
        ));
    }
    if stop_evidence.last_host_sentinel_probe_kind != Some(HostSentinelProbeKind::NativeMacHost) {
        return Err(HarnessError::invalid_state(
            "native host-sentinel runner Stop must record last probe kind NativeMacHost",
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
        host_sentinel_probes_performed: stop_evidence.host_sentinel_probes_performed,
        last_host_sentinel_probe_kind: stop_evidence.last_host_sentinel_probe_kind,
        vf_dry_run: None,
        nonclaim: NATIVE_HOST_SENTINEL_NONCLAIM.into(),
        recorded_at: Utc::now(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_checkout_is_rejected_before_platform_branch() {
        let err = run_native_host_sentinel(Path::new(""), None).expect_err("checkout required");
        assert_eq!(err.code, crate::error::HarnessErrorCode::InvalidState);
        assert!(err.message.contains("explicitly supplied checkout PATH"));
    }

    #[test]
    fn macos_fail_closed_is_never_non_macos_or_unsupported() {
        let evidence = NativeSentinelEvidence::macos_fail_closed(
            NativeSentinelRunnerOutcome::BackendUnavailable,
            "collector unavailable",
        );
        assert_eq!(evidence.platform, NativeSentinelRunnerPlatform::MacOs);
        assert_ne!(
            evidence.outcome,
            NativeSentinelRunnerOutcome::UnsupportedPlatform
        );
        assert!(!evidence.synthetic_fallback_used);
        assert!(!evidence.live_host_sentinel_collection);
        assert_eq!(
            evidence.collector_error.as_deref(),
            Some("collector unavailable")
        );
    }

    #[test]
    fn linux_native_runner_is_unsupported_without_synthetic_fallback() {
        let evidence =
            run_native_host_sentinel(Path::new("/explicit/checkout"), None).expect("artifact");
        let vf = evidence.vf_dry_run.as_ref().expect("native+VF nested");
        assert!(vf.native_host_sentinels_requested);
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(evidence.platform, NativeSentinelRunnerPlatform::NonMacOs);
            assert_eq!(
                evidence.outcome,
                NativeSentinelRunnerOutcome::UnsupportedPlatform
            );
            assert_eq!(vf.platform, VfDryRunPlatform::NonMacOs);
            assert_eq!(vf.outcome, VfDryRunOutcome::UnsupportedPlatform);
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
            assert_ne!(vf.platform, VfDryRunPlatform::NonMacOs);
            assert_ne!(vf.outcome, VfDryRunOutcome::UnsupportedPlatform);
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
                    assert_eq!(
                        evidence.last_host_sentinel_probe_kind,
                        Some(HostSentinelProbeKind::NativeMacHost)
                    );
                }
                NativeSentinelRunnerOutcome::UnsupportedPlatform => {
                    panic!("macOS native runner must not report NonMacOs unsupported");
                }
            }
        }
    }
}
