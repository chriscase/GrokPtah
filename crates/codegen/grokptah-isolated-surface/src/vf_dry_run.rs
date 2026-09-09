//! Platform-neutral VF dry-run entry points for the Sep 18 sequencer.
//!
//! Linux CI and macOS builds without `vf-backend` receive honest Unsupported /
//! Unavailable artifacts — never a physical PASS claim. The macOS `vf-backend`
//! rehearsal path invokes `MacHostSentinelCollector` so sealed evidence can
//! distinguish native host observation from [`SyntheticHostProbe`] self-compare.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::VfLaunchReceipt;
use crate::error::{HarnessError, HarnessResult};
use crate::lifecycle::ProofEvidenceClass;
use crate::sentinel::{HostSentinelProbeKind, HostSentinelSnapshot};
use crate::VF_DRY_RUN_NONCLAIM;

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::error::HarnessErrorCode;
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::harness::IsolatedSurfaceHarness;
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::sentinel::MacHostSentinelCollector;
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

/// How host sentinels were observed during a VF dry-run.
///
/// Missing on old records so they deserialize as synthetic self-compare and
/// cannot claim native Mac collection by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VfDryRunHostObservationKind {
    /// Linux CI / feature-disabled rehearsal — no Mac collector.
    #[default]
    SyntheticHostProbeSelfCompare,
    /// `MacHostSentinelCollector` was the harness probe on macOS `vf-backend`.
    NativeMacHostCollector,
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
    /// Native Mac collector vs [`SyntheticHostProbe`] self-compare.
    #[serde(default)]
    pub host_observation_kind: VfDryRunHostObservationKind,
    /// True only when `MacHostSentinelCollector::collect` ran on this path.
    #[serde(default)]
    pub native_collector_invoked: bool,
    /// True only after a successful native probe match — never physical PASS.
    #[serde(default)]
    pub live_host_sentinel_collection: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_collect_error: Option<HarnessError>,
    #[serde(default)]
    pub host_sentinel_probes_performed: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_host_sentinel_probe_kind: Option<HostSentinelProbeKind>,
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
            host_observation_kind: VfDryRunHostObservationKind::SyntheticHostProbeSelfCompare,
            native_collector_invoked: false,
            live_host_sentinel_collection: false,
            native_collect_error: None,
            host_sentinel_probes_performed: 0,
            last_host_sentinel_probe_kind: None,
        }
    }

    /// Linux / feature-disabled VF dry-run is synthetic self-compare, not native.
    pub fn is_synthetic_host_self_compare(&self) -> bool {
        self.host_observation_kind == VfDryRunHostObservationKind::SyntheticHostProbeSelfCompare
            && !self.native_collector_invoked
            && !self.live_host_sentinel_collection
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
    let collector =
        MacHostSentinelCollector::new(baseline.main_checkout_fence.checkout_path.clone());
    let native_collect = collector.collect();
    let native_collect_error = native_collect.as_ref().err().cloned();
    let harness_baseline = native_collect.unwrap_or(baseline);

    let backend = VirtualizationFrameworkBackend::new();
    let mut harness = IsolatedSurfaceHarness::with_vf_backend(harness_baseline, backend, receipt)?
        .with_native_mac_host_collector(collector);
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::VirtualizationFramework
    );

    let boot_err = harness
        .boot()
        .expect_err("VF dry-run boot must fail closed");
    if boot_err.code != HarnessErrorCode::BackendUnavailable
        && boot_err.code != HarnessErrorCode::HostSentinelViolation
    {
        return Err(HarnessError::invalid_state(format!(
            "VF dry-run boot must return BackendUnavailable or HostSentinelViolation, got {:?}",
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
        host_observation_kind: VfDryRunHostObservationKind::NativeMacHostCollector,
        native_collector_invoked: true,
        live_host_sentinel_collection: stop_evidence.live_host_sentinel_collection,
        native_collect_error,
        host_sentinel_probes_performed: stop_evidence.host_sentinel_probes_performed,
        last_host_sentinel_probe_kind: stop_evidence.last_host_sentinel_probe_kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_observation_fields_deserialize_as_synthetic_self_compare() {
        let json = r#"{
            "platform": "non_mac_os",
            "outcome": "unsupported_platform",
            "evidenceClass": "virtualization_framework",
            "bootAttempted": false,
            "physicalPassClaimed": false,
            "nonclaim": "legacy",
            "recordedAt": "2026-09-09T00:00:00Z"
        }"#;
        let evidence: VfDryRunEvidence = serde_json::from_str(json).expect("legacy pack");
        assert!(evidence.is_synthetic_host_self_compare());
        assert_eq!(
            evidence.host_observation_kind,
            VfDryRunHostObservationKind::SyntheticHostProbeSelfCompare
        );
        assert!(!evidence.native_collector_invoked);
        assert!(!evidence.live_host_sentinel_collection);
        assert_eq!(evidence.last_host_sentinel_probe_kind, None);
    }
}
