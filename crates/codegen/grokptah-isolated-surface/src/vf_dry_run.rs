//! Platform-neutral VF dry-run entry points for the Sep 18 sequencer.
//!
//! Linux CI and macOS builds without `vf-backend` receive honest Unsupported /
//! Unavailable artifacts — never a physical PASS claim. Native host-sentinel
//! mode attaches [`crate::sentinel::MacHostSentinelCollector`] before VF boot,
//! lifecycle, and Stop. Default VF dry-run (no native flag) still uses
//! synthetic host probes.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::backend::VfLaunchReceipt;
use crate::error::{HarnessError, HarnessResult};
#[cfg(all(target_os = "macos", feature = "vf-backend"))]
use crate::harness::StopEvidence;
use crate::lifecycle::ProofEvidenceClass;
use crate::sentinel::{HostSentinelProbeKind, HostSentinelSnapshot};
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
    /// True when `--native-host-sentinels` requested native collector attach.
    #[serde(default)]
    pub native_host_sentinels_requested: bool,
    /// Actual Stop probe count. Never a hardcoded zero when Stop ran.
    #[serde(default)]
    pub host_sentinel_probes_performed: u32,
    #[serde(default)]
    pub live_host_sentinel_collection_at_stop: bool,
    #[serde(default)]
    pub host_sentinels_unchanged_at_stop: bool,
    #[serde(default)]
    pub channels_destroyed: usize,
    #[serde(default)]
    pub channels_open_after_stop: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_host_sentinel_probe_kind: Option<HostSentinelProbeKind>,
}

impl VfDryRunEvidence {
    /// Construct a fail-closed VF dry-run artifact for callers and integration
    /// tests that must exercise the serialized evidence contract. This never
    /// represents boot success or physical qualification.
    pub fn fail_closed(platform: VfDryRunPlatform, outcome: VfDryRunOutcome) -> Self {
        Self::unsupported(platform, outcome)
    }

    pub(crate) fn unsupported(platform: VfDryRunPlatform, outcome: VfDryRunOutcome) -> Self {
        Self {
            platform,
            outcome,
            evidence_class: ProofEvidenceClass::VirtualizationFramework,
            boot_attempted: false,
            physical_pass_claimed: false,
            nonclaim: VF_DRY_RUN_NONCLAIM.into(),
            recorded_at: Utc::now(),
            native_host_sentinels_requested: false,
            host_sentinel_probes_performed: 0,
            live_host_sentinel_collection_at_stop: false,
            host_sentinels_unchanged_at_stop: false,
            channels_destroyed: 0,
            channels_open_after_stop: 0,
            last_host_sentinel_probe_kind: None,
        }
    }

    pub(crate) fn with_native_requested(mut self) -> Self {
        self.native_host_sentinels_requested = true;
        self
    }

    #[cfg(all(target_os = "macos", feature = "vf-backend"))]
    pub(crate) fn with_stop_summary(mut self, stop: &StopEvidence) -> Self {
        self.host_sentinel_probes_performed = stop.host_sentinel_probes_performed;
        self.live_host_sentinel_collection_at_stop = stop.live_host_sentinel_collection;
        self.host_sentinels_unchanged_at_stop = stop.host_sentinels_unchanged;
        self.channels_destroyed = stop.channels_destroyed;
        self.channels_open_after_stop = 0;
        self.last_host_sentinel_probe_kind = stop.last_host_sentinel_probe_kind;
        self
    }
}

/// Run the bounded VF dry-run path for the current build target.
pub fn run_vf_dry_run(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    run_vf_dry_run_inner(receipt, baseline, false, None)
}

/// VF dry-run with exclusive native Mac host-sentinel collection.
///
/// Requires an explicitly supplied checkout path. Attaches
/// [`crate::sentinel::MacHostSentinelCollector`] before VF boot/lifecycle/Stop
/// on macOS + `vf-backend`. Never falls back to synthetic probes when native
/// was requested.
pub fn run_vf_dry_run_with_native_host_sentinels(
    receipt: VfLaunchReceipt,
    checkout_path: &Path,
) -> HarnessResult<VfDryRunEvidence> {
    if checkout_path.as_os_str().is_empty() {
        return Err(HarnessError::invalid_state(
            "native host-sentinel VF dry-run requires an explicitly supplied checkout PATH",
        ));
    }
    run_vf_dry_run_inner(
        receipt,
        HostSentinelSnapshot::synthetic_baseline(),
        true,
        Some(checkout_path),
    )
}

fn run_vf_dry_run_inner(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
    checkout_path: Option<&Path>,
) -> HarnessResult<VfDryRunEvidence> {
    if receipt.physical_mac_proof_id.trim().is_empty() {
        return Err(HarnessError::invalid_state(
            "VF dry-run requires a non-empty physical_mac_proof_id on the launch receipt",
        ));
    }

    run_vf_dry_run_impl(receipt, baseline, native_host_sentinels, checkout_path)
}

#[cfg(not(target_os = "macos"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
    checkout_path: Option<&Path>,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline, checkout_path);
    let mut evidence = VfDryRunEvidence::unsupported(
        VfDryRunPlatform::NonMacOs,
        VfDryRunOutcome::UnsupportedPlatform,
    );
    if native_host_sentinels {
        evidence = evidence.with_native_requested();
    }
    Ok(evidence)
}

#[cfg(all(target_os = "macos", not(feature = "vf-backend")))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
    checkout_path: Option<&Path>,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline, checkout_path);
    let mut evidence = VfDryRunEvidence::unsupported(
        VfDryRunPlatform::MacOsFeatureDisabled,
        VfDryRunOutcome::FeatureDisabled,
    );
    if native_host_sentinels {
        evidence = evidence.with_native_requested();
    }
    Ok(evidence)
}

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
    checkout_path: Option<&Path>,
) -> HarnessResult<VfDryRunEvidence> {
    let backend = VirtualizationFrameworkBackend::new();
    let mut harness = if native_host_sentinels {
        let checkout = checkout_path.ok_or_else(|| {
            HarnessError::invalid_state(
                "native host-sentinel VF dry-run requires an explicitly supplied checkout PATH",
            )
        })?;
        let collector =
            crate::sentinel::MacHostSentinelCollector::new(checkout.display().to_string());
        let native_baseline = match collector.collect() {
            Ok(snapshot) => snapshot,
            Err(_err) => {
                return Ok(VfDryRunEvidence::unsupported(
                    VfDryRunPlatform::MacOsDryRun,
                    VfDryRunOutcome::BackendUnavailable,
                )
                .with_native_requested());
            }
        };
        let mut harness =
            IsolatedSurfaceHarness::with_vf_backend(native_baseline, backend, receipt)?;
        harness.attach_native_collector(collector)?;
        harness
    } else {
        IsolatedSurfaceHarness::with_vf_backend(baseline, backend, receipt)?
    };

    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::VirtualizationFramework
    );

    match harness.boot() {
        Ok(()) => {
            return Err(HarnessError::invalid_state(
                "VF dry-run boot must fail closed",
            ));
        }
        Err(err)
            if err.code == HarnessErrorCode::BackendUnavailable
                || err.code == HarnessErrorCode::HostSentinelViolation => {}
        Err(err) => {
            return Err(HarnessError::invalid_state(format!(
                "VF dry-run boot must return BackendUnavailable, got {:?}",
                err.code
            )));
        }
    }

    let stop_evidence = harness.stop()?;
    if stop_evidence.disposition.is_none() {
        return Err(HarnessError::invalid_state(
            "VF dry-run stop must record a disposition",
        ));
    }

    let mut evidence = VfDryRunEvidence {
        platform: VfDryRunPlatform::MacOsDryRun,
        outcome: VfDryRunOutcome::BackendUnavailable,
        evidence_class: ProofEvidenceClass::VirtualizationFramework,
        boot_attempted: true,
        physical_pass_claimed: false,
        nonclaim: VF_DRY_RUN_NONCLAIM.into(),
        recorded_at: Utc::now(),
        native_host_sentinels_requested: native_host_sentinels,
        host_sentinel_probes_performed: 0,
        live_host_sentinel_collection_at_stop: false,
        host_sentinels_unchanged_at_stop: false,
        channels_destroyed: 0,
        channels_open_after_stop: 0,
        last_host_sentinel_probe_kind: None,
    };
    evidence = evidence.with_stop_summary(&stop_evidence);
    Ok(evidence)
}
