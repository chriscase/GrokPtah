//! Platform-neutral VF dry-run entry points for the Sep 18 sequencer.
//!
//! Linux CI and macOS builds without `vf-backend` receive honest Unsupported /
//! Unavailable artifacts — never a physical PASS claim.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::VfLaunchReceipt;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::sentinel::HostSentinelSnapshot;
use crate::VF_DRY_RUN_NONCLAIM;

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::error::HarnessErrorCode;
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::harness::IsolatedSurfaceHarness;
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::vf_backend::VirtualizationFrameworkBackend;

/// Where the VF dry-run was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfDryRunPlatform {
    NonMacOs,
    MacOsFeatureDisabled,
    MacOsDryRun,
}

/// Outcome of a VF dry-run attempt. Never represents a physical Sep 18 PASS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfDryRunOutcome {
    UnsupportedPlatform,
    FeatureDisabled,
    BackendUnavailable,
}

/// Honest artifact from [`Sep18NoModelProofSequencer::run_vf_dry_run`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VfDryRunEvidence {
    pub platform: VfDryRunPlatform,
    pub outcome: VfDryRunOutcome,
    pub evidence_class: ProofEvidenceClass,
    pub boot_attempted: bool,
    pub physical_pass_claimed: bool,
    pub nonclaim: String,
    pub recorded_at: DateTime<Utc>,
}

impl VfDryRunEvidence {
    fn unsupported(platform: VfDryRunPlatform, outcome: VfDryRunOutcome) -> Self {
        Self {
            platform,
            outcome,
            evidence_class: ProofEvidenceClass::VirtualizationFramework,
            boot_attempted: false,
            physical_pass_claimed: false,
            nonclaim: VF_DRY_RUN_NONCLAIM.into(),
            recorded_at: Utc::now(),
        }
    }
}

/// Run the bounded VF dry-run path for the current build target.
pub fn run_vf_dry_run(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    if receipt.physical_mac_proof_id.trim().is_empty() {
        return Err(HarnessError::invalid_state(
            "VF dry-run requires a non-empty physical_mac_proof_id on the launch receipt",
        ));
    }

    run_vf_dry_run_impl(receipt, baseline)
}

#[cfg(not(target_os = "macos"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline);
    Ok(VfDryRunEvidence::unsupported(
        VfDryRunPlatform::NonMacOs,
        VfDryRunOutcome::UnsupportedPlatform,
    ))
}

#[cfg(all(target_os = "macos", not(feature = "vf-backend")))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline);
    Ok(VfDryRunEvidence::unsupported(
        VfDryRunPlatform::MacOsFeatureDisabled,
        VfDryRunOutcome::FeatureDisabled,
    ))
}

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    let backend = VirtualizationFrameworkBackend::new();
    let mut harness = IsolatedSurfaceHarness::with_vf_backend(baseline, backend, receipt)?;
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::VirtualizationFramework
    );

    let boot_err = harness
        .boot()
        .expect_err("VF dry-run boot must fail closed");
    if boot_err.code != HarnessErrorCode::BackendUnavailable {
        return Err(HarnessError::invalid_state(format!(
            "VF dry-run boot must return BackendUnavailable, got {:?}",
            boot_err.code
        )));
    }

    let stop_evidence = harness.stop()?;
    if stop_evidence.disposition.is_none() {
        return Err(HarnessError::invalid_state(
            "VF dry-run stop must record a disposition",
        ));
    }

    Ok(VfDryRunEvidence {
        platform: VfDryRunPlatform::MacOsDryRun,
        outcome: VfDryRunOutcome::BackendUnavailable,
        evidence_class: ProofEvidenceClass::VirtualizationFramework,
        boot_attempted: true,
        physical_pass_claimed: false,
        nonclaim: VF_DRY_RUN_NONCLAIM.into(),
        recorded_at: Utc::now(),
    })
}
