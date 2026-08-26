//! Read-only availability report for isolated visual Computer Use.
//!
//! This is the substrate's only public surface, and it is deliberately not a
//! re-export of the runtime: no lifecycle, lease, channel, frame, input,
//! protocol, helper, or supervisor type crosses this boundary. A caller can
//! learn *whether* isolated visual Computer Use is dispatchable and *why not*,
//! and nothing else. Calling it starts no guest, spawns no helper, opens no
//! descriptor, and mints no Computer Use authority.

use serde::Serialize;

use super::isolated_visual_harness::run_real_measured_launch;
use super::isolated_visual_selfcheck::run_isolated_visual_selfcheck;
use super::isolated_visual_soak::hardware_soak_startable;

/// One reason isolated visual Computer Use is not dispatchable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerIsolatedVisualBlocker {
    /// The reviewed dispatch path does not exist yet. This blocker is removed
    /// only by a separate reviewed change, never by configuration.
    DispatchDisabled,
    /// The host is not a platform the packaged supervisor can run on.
    UnsupportedPlatform,
    /// No correctly signed helper/guest package has been measured on this host.
    PackagedHelperUnverified,
    /// The measured real-hardware boot/frame/input gates have not passed.
    MeasuredHardwareGatesUnmet,
    /// Independent security review has not signed off.
    IndependentSecurityReviewPending,
    /// Independent accessibility review has not signed off.
    IndependentAccessibilityReviewPending,
    /// The substrate contract self-check did not hold in this build.
    ContractSelfCheckFailed,
}

/// Secret-free availability report. Paths, digests, descriptors, challenges,
/// and process handles are deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerIsolatedVisualStatus {
    /// Always `false` in this build. Isolated visual Computer Use is not
    /// dispatchable and cannot be enabled by configuration.
    pub dispatch_enabled: bool,
    /// Whether the substrate's own contract rehearsal held in this build.
    pub contract_self_check_passed: bool,
    /// The furthest stage a measured launch has been observed to reach on
    /// this host. `not_attempted` means no guest, helper, or package has
    /// been touched at all.
    pub measured_launch_stage: &'static str,
    /// Whether a qualifying hardware soak could be started on this host.
    /// False until a measured launch has actually reached stop and reap.
    pub hardware_soak_startable: bool,
    /// Every reason dispatch is unavailable, in a stable order.
    pub blockers: Vec<ComputerIsolatedVisualBlocker>,
}

/// Reports isolated visual availability. Fail-closed by construction: the
/// blocker list is never empty in this build, and `dispatch_enabled` is a
/// constant `false` rather than a computed value a caller could flip.
pub fn computer_isolated_visual_status() -> ComputerIsolatedVisualStatus {
    let contract_self_check_passed = run_isolated_visual_selfcheck().is_ok();
    // Reported without an operator opt-in, so this observes the harness
    // refusing rather than attempting anything.
    let measured_launch = run_real_measured_launch(None);

    let mut blockers = vec![ComputerIsolatedVisualBlocker::DispatchDisabled];
    if !cfg!(target_os = "macos") {
        blockers.push(ComputerIsolatedVisualBlocker::UnsupportedPlatform);
    }
    blockers.push(ComputerIsolatedVisualBlocker::PackagedHelperUnverified);
    blockers.push(ComputerIsolatedVisualBlocker::MeasuredHardwareGatesUnmet);
    blockers.push(ComputerIsolatedVisualBlocker::IndependentSecurityReviewPending);
    blockers.push(ComputerIsolatedVisualBlocker::IndependentAccessibilityReviewPending);
    if !contract_self_check_passed {
        blockers.push(ComputerIsolatedVisualBlocker::ContractSelfCheckFailed);
    }

    ComputerIsolatedVisualStatus {
        dispatch_enabled: false,
        contract_self_check_passed,
        measured_launch_stage: measured_launch.launch_attempted.as_str(),
        hardware_soak_startable: hardware_soak_startable(),
        blockers,
    }
}

#[cfg(test)]
mod tests {
    use super::super::isolated_visual_harness::LaunchStage;
    use super::*;

    #[test]
    fn status_is_fail_closed_and_self_check_holds() {
        let status = computer_isolated_visual_status();
        assert!(!status.dispatch_enabled);
        assert!(status.contract_self_check_passed);
        assert!(status
            .blockers
            .contains(&ComputerIsolatedVisualBlocker::DispatchDisabled));
        assert!(!status
            .blockers
            .contains(&ComputerIsolatedVisualBlocker::ContractSelfCheckFailed));
        assert!(status
            .blockers
            .contains(&ComputerIsolatedVisualBlocker::PackagedHelperUnverified));
        assert!(status
            .blockers
            .contains(&ComputerIsolatedVisualBlocker::MeasuredHardwareGatesUnmet));
        assert_eq!(
            status.measured_launch_stage,
            LaunchStage::NotAttempted.as_str(),
            "status must not report a launch stage it did not reach"
        );
        assert!(
            !status.hardware_soak_startable,
            "a soak must not be startable before the hardware gates pass"
        );
    }

    #[test]
    fn status_projection_carries_no_substrate_detail() {
        let encoded = serde_json::to_string(&computer_isolated_visual_status()).unwrap();
        for needle in [
            "challenge",
            "secret",
            "digest",
            "sha256",
            "path",
            "helper_path",
            "overlay",
            "lease",
            "descriptor",
            "pid",
        ] {
            assert!(
                !encoded.contains(needle),
                "isolated visual status leaked {needle}: {encoded}"
            );
        }
    }
}
