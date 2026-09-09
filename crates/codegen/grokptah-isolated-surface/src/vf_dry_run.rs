//! Platform-neutral VF dry-run entry points for the Sep 18 sequencer.
//!
//! Linux CI and macOS builds without `vf-backend` receive honest Unsupported /
//! Unavailable artifacts — never a physical PASS claim. Native host sentinel
//! collection is opt-in: the synthetic rehearsal probe is used only when native
//! mode was not requested. Once native is requested, unavailable / error /
//! incomplete readings fail closed with no synthetic fallback.

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
use crate::lifecycle::GuestLifecyclePhase;
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
    /// True only when `--native-host-sentinels` / native mode was requested.
    #[serde(default)]
    pub native_host_sentinels_requested: bool,
    /// Stop-probe provenance. Existing [`HostSentinelProbeKind`] — never clipboard,
    /// window titles, paths, secrets, or handles.
    #[serde(default)]
    pub host_sentinel_probe_kind: Option<HostSentinelProbeKind>,
    #[serde(default)]
    pub host_sentinel_probes_performed: u32,
    #[serde(default)]
    pub host_sentinels_unchanged_at_stop: bool,
    #[serde(default)]
    pub live_host_sentinel_collection_at_stop: bool,
    #[serde(default)]
    pub channels_destroyed: usize,
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
            native_host_sentinels_requested: false,
            host_sentinel_probe_kind: None,
            host_sentinel_probes_performed: 0,
            host_sentinels_unchanged_at_stop: false,
            live_host_sentinel_collection_at_stop: false,
            channels_destroyed: 0,
        }
    }
}

/// Run the bounded VF dry-run path for the current build target.
///
/// Uses the synthetic host sentinel probe unless native collection is requested.
pub fn run_vf_dry_run(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
) -> HarnessResult<VfDryRunEvidence> {
    run_vf_dry_run_with_native_host_sentinels(receipt, baseline, false)
}

/// VF dry-run with optional native host sentinel collection.
///
/// Synthetic rehearsal is the default only when `native_host_sentinels` is false.
/// Once native is requested, unavailable / error / incomplete readings fail closed
/// with no synthetic fallback.
pub fn run_vf_dry_run_with_native_host_sentinels(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
) -> HarnessResult<VfDryRunEvidence> {
    if receipt.physical_mac_proof_id.trim().is_empty() {
        return Err(HarnessError::invalid_state(
            "VF dry-run requires a non-empty physical_mac_proof_id on the launch receipt",
        ));
    }

    if native_host_sentinels {
        fail_closed_if_native_unavailable()?;
    }

    run_vf_dry_run_impl(receipt, baseline, native_host_sentinels)
}

fn fail_closed_if_native_unavailable() -> HarnessResult<()> {
    if !cfg!(target_os = "macos") {
        return Err(HarnessError::backend_unavailable(
            "native host sentinel collector is unavailable on this platform",
        ));
    }
    if !cfg!(feature = "vf-backend") {
        return Err(HarnessError::backend_unavailable(
            "native host sentinel VF dry-run requires the vf-backend feature",
        ));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline);
    if native_host_sentinels {
        return Err(HarnessError::backend_unavailable(
            "native host sentinel collector is unavailable on this platform",
        ));
    }
    Ok(VfDryRunEvidence::unsupported(
        VfDryRunPlatform::NonMacOs,
        VfDryRunOutcome::UnsupportedPlatform,
    ))
}

#[cfg(all(target_os = "macos", not(feature = "vf-backend")))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
) -> HarnessResult<VfDryRunEvidence> {
    let _ = (receipt, baseline);
    if native_host_sentinels {
        return Err(HarnessError::backend_unavailable(
            "native host sentinel VF dry-run requires the vf-backend feature",
        ));
    }
    Ok(VfDryRunEvidence::unsupported(
        VfDryRunPlatform::MacOsFeatureDisabled,
        VfDryRunOutcome::FeatureDisabled,
    ))
}

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
fn run_vf_dry_run_impl(
    receipt: VfLaunchReceipt,
    baseline: HostSentinelSnapshot,
    native_host_sentinels: bool,
) -> HarnessResult<VfDryRunEvidence> {
    let backend = VirtualizationFrameworkBackend::new();
    let mut harness = if native_host_sentinels {
        let collector = native_collector()?;
        let native_baseline = collector.collect()?;
        if !native_baseline.native_reading_is_complete() {
            return Err(HarnessError::backend_unavailable(
                "native host sentinel reading is incomplete",
            ));
        }
        IsolatedSurfaceHarness::with_vf_backend(native_baseline, backend, receipt)?
            .attach_native_host_sentinel_collector(collector)
    } else {
        IsolatedSurfaceHarness::with_vf_backend(baseline, backend, receipt)?
    };
    assert_eq!(
        harness.evidence_class(),
        ProofEvidenceClass::VirtualizationFramework
    );

    let boot_err = harness
        .boot()
        .expect_err("VF dry-run boot must fail closed");
    if native_host_sentinels && harness.lifecycle().phase == GuestLifecyclePhase::NotStarted {
        return Err(boot_err);
    }
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
        native_host_sentinels_requested: native_host_sentinels,
        host_sentinel_probe_kind: stop_evidence.host_sentinel_probe_kind,
        host_sentinel_probes_performed: stop_evidence.host_sentinel_probes_performed,
        host_sentinels_unchanged_at_stop: stop_evidence.host_sentinels_unchanged,
        live_host_sentinel_collection_at_stop: stop_evidence.live_host_sentinel_collection,
        channels_destroyed: stop_evidence.channels_destroyed,
    })
}

#[cfg(all(target_os = "macos", feature = "vf-backend"))]
fn native_collector() -> HarnessResult<MacHostSentinelCollector> {
    let checkout = std::env::current_dir().map_err(|_| {
        HarnessError::backend_unavailable(
            "native host sentinel collector cannot resolve the checkout",
        )
    })?;
    Ok(MacHostSentinelCollector::new(
        checkout.to_string_lossy().into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::HarnessErrorCode;

    fn receipt() -> VfLaunchReceipt {
        VfLaunchReceipt {
            physical_mac_proof_id: "native-host-sentinel-dry-run".into(),
        }
    }

    #[test]
    fn synthetic_default_when_native_not_requested() {
        let evidence = run_vf_dry_run(receipt(), HostSentinelSnapshot::synthetic_baseline())
            .expect("synthetic VF dry-run");
        assert!(!evidence.native_host_sentinels_requested);
        assert!(!evidence.physical_pass_claimed);
        assert_eq!(evidence.host_sentinel_probe_kind, None);
        assert_eq!(evidence.nonclaim, VF_DRY_RUN_NONCLAIM);
    }

    #[cfg(not(all(target_os = "macos", feature = "vf-backend")))]
    #[test]
    fn native_requested_fails_closed_without_synthetic_fallback() {
        let err = run_vf_dry_run_with_native_host_sentinels(
            receipt(),
            HostSentinelSnapshot::synthetic_baseline(),
            true,
        )
        .expect_err("native requested must fail closed when collector is unavailable");
        assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("native host sentinel"));
        assert!(!err.message.contains("clipboard"));
        assert!(!err.message.contains("AX"));
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn harness_require_native_host_sentinels_fails_closed() {
        use crate::harness::IsolatedSurfaceHarness;
        let harness = IsolatedSurfaceHarness::new(HostSentinelSnapshot::synthetic_baseline());
        let err = harness
            .require_native_host_sentinels()
            .err()
            .expect("native must fail closed off macOS");
        assert_eq!(err.code, HarnessErrorCode::BackendUnavailable);
        assert!(err.message.contains("unavailable"));
    }

    #[test]
    fn native_requested_still_rejects_empty_receipt_before_side_effects() {
        let err = run_vf_dry_run_with_native_host_sentinels(
            VfLaunchReceipt {
                physical_mac_proof_id: "  ".into(),
            },
            HostSentinelSnapshot::synthetic_baseline(),
            true,
        )
        .expect_err("empty receipt");
        assert_eq!(err.code, HarnessErrorCode::InvalidState);
    }
}
