//! Host sentinel snapshots for noninterference proof (#288/#286).
//!
//! The synthetic harness records pointer, foreground, clipboard, and unrelated
//! window state at harness start. **Unchanged** is only claimable after at least
//! one real host probe compares observed state to the baseline — never a no-op
//! self-compare of an uninitialized `current` field.

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

/// Digest or mtime fence for the main checkout (not the disposable worktree).
/// Detects synthetic mutation of the host main checkout during harness runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MainCheckoutFence {
    pub checkout_path: String,
    pub content_digest: String,
    pub mtime_fence_ns: Option<u64>,
}

impl MainCheckoutFence {
    pub fn synthetic() -> Self {
        Self {
            checkout_path: "/workspace".into(),
            content_digest: "sha256:main-checkout-baseline".into(),
            mtime_fence_ns: Some(1_700_000_000_000_000_000),
        }
    }
}

/// Immutable host-side sentinel values captured before guest boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelSnapshot {
    pub pointer_x: i32,
    pub pointer_y: i32,
    pub foreground_app_id: String,
    pub foreground_window_id: String,
    pub clipboard_digest: String,
    pub unrelated_window_app_id: String,
    pub unrelated_window_id: String,
    pub unrelated_window_title_hash: String,
    pub main_checkout_fence: MainCheckoutFence,
}

impl HostSentinelSnapshot {
    pub fn synthetic_baseline() -> Self {
        Self {
            pointer_x: 640,
            pointer_y: 400,
            foreground_app_id: "com.grokptah.codex".into(),
            foreground_window_id: "codex-main-1".into(),
            clipboard_digest: "sha256:empty-clipboard-baseline".into(),
            unrelated_window_app_id: "com.apple.finder".into(),
            unrelated_window_id: "finder-desktop-1".into(),
            unrelated_window_title_hash: "sha256:finder-desktop".into(),
            main_checkout_fence: MainCheckoutFence::synthetic(),
        }
    }
}

/// Provenance for host sentinel probes — physical proof requires native reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostSentinelProbeKind {
    /// Harness/rehearsal self-compare only — ineligible for physical Mac markers.
    SyntheticRehearsal,
    /// Live Mac host read via [`MacHostSentinelCollector`].
    NativeMacHost,
}

/// Reads live host sentinel state. Native Mac adapters implement this with real
/// AX/CGEvent/clipboard evidence and main-checkout digest/mtime fence; the
/// synthetic harness uses [`SyntheticHostProbe`].
pub trait HostSentinelProbe {
    fn probe_host(&self) -> HarnessResult<HostSentinelSnapshot>;

    fn probe_kind(&self) -> HostSentinelProbeKind {
        HostSentinelProbeKind::SyntheticRehearsal
    }
}

/// Mac physical proof hook for live host sentinel collection.
///
/// On Sep 18, a Mac worker collects pointer, foreground app/window, clipboard digest,
/// unrelated host window, and `MainCheckoutFence`, then passes the snapshot to
/// [`HostSentinelRegistry::refresh_from_native_collector`] (or
/// [`IsolatedSurfaceHarness::refresh_host_sentinels_from_collector`]) at boot,
/// inject, and Stop probe points. Fails closed when Accessibility trust or other
/// required host reads are unavailable.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct MacHostSentinelCollector {
    checkout_path: String,
}

#[cfg(target_os = "macos")]
impl MacHostSentinelCollector {
    pub fn new(checkout_path: impl Into<String>) -> Self {
        Self {
            checkout_path: checkout_path.into(),
        }
    }

    pub fn checkout_path(&self) -> &str {
        &self.checkout_path
    }

    /// Collect live host sentinel state for [`HostSentinelRegistry::refresh_from_host`].
    pub fn collect(&self) -> HarnessResult<HostSentinelSnapshot> {
        crate::mac_host_sentinel::collect(&self.checkout_path)
    }
}

#[cfg(target_os = "macos")]
impl HostSentinelProbe for MacHostSentinelCollector {
    fn probe_host(&self) -> HarnessResult<HostSentinelSnapshot> {
        self.collect()
    }

    fn probe_kind(&self) -> HostSentinelProbeKind {
        HostSentinelProbeKind::NativeMacHost
    }
}

/// Synthetic host-side state for harness tests and rehearsal only.
///
/// Self-compare against the harness baseline is **not** physical Mac proof.
#[derive(Debug, Clone)]
pub struct SyntheticHostProbe {
    state: HostSentinelSnapshot,
}

impl SyntheticHostProbe {
    /// Explicit rehearsal label — not eligible for physical proof markers.
    pub const PROBE_LABEL: &'static str = "synthetic-rehearsal-only-not-physical-proof";
}

impl SyntheticHostProbe {
    pub fn new(baseline: HostSentinelSnapshot) -> Self {
        Self { state: baseline }
    }

    /// Test hook: simulate host mutation that a real probe must detect.
    pub fn simulate_host_mutation(&mut self, field: &str) {
        match field {
            "pointer" => {
                self.state.pointer_x += 1;
            }
            "foreground" => {
                self.state.foreground_app_id = "com.host.mutated".into();
            }
            "clipboard" => {
                self.state.clipboard_digest = "sha256:clipboard-changed".into();
            }
            "unrelated_window" => {
                self.state.unrelated_window_title_hash = "sha256:mutated".into();
            }
            "main_checkout" | "main_checkout_digest" => {
                self.state.main_checkout_fence.content_digest =
                    "sha256:main-checkout-mutated".into();
            }
            "main_checkout_mtime" => {
                self.state.main_checkout_fence.mtime_fence_ns =
                    Some(self.state.main_checkout_fence.mtime_fence_ns.unwrap_or(0) + 1);
            }
            _ => {
                self.state.pointer_y += 1;
            }
        }
    }
}

impl HostSentinelProbe for SyntheticHostProbe {
    fn probe_host(&self) -> HarnessResult<HostSentinelSnapshot> {
        Ok(self.state.clone())
    }

    fn probe_kind(&self) -> HostSentinelProbeKind {
        HostSentinelProbeKind::SyntheticRehearsal
    }
}

/// Tracks host sentinel drift during a harness run. Guest operations must never
/// mutate host sentinels — violations fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelRegistry {
    baseline: HostSentinelSnapshot,
    violation: Option<String>,
    probes_performed: u32,
    native_host_probes_performed: u32,
    last_probe_matches_baseline: bool,
    /// Kind of the most recent probe comparison. Missing on old records so they
    /// cannot deserialize into a live collection claim.
    #[serde(default)]
    last_probe_kind: Option<HostSentinelProbeKind>,
}

impl HostSentinelRegistry {
    pub fn capture(baseline: HostSentinelSnapshot) -> Self {
        Self {
            baseline,
            violation: None,
            probes_performed: 0,
            native_host_probes_performed: 0,
            last_probe_matches_baseline: false,
            last_probe_kind: None,
        }
    }

    pub fn baseline(&self) -> &HostSentinelSnapshot {
        &self.baseline
    }

    pub fn probes_performed(&self) -> u32 {
        self.probes_performed
    }

    pub fn native_host_probes_performed(&self) -> u32 {
        self.native_host_probes_performed
    }

    /// Kind of the most recent probe comparison, if any probe has run.
    pub fn last_probe_kind(&self) -> Option<HostSentinelProbeKind> {
        self.last_probe_kind
    }

    /// True only when the latest completed probe was a successful native Mac host
    /// read that matched baseline. A later synthetic self-compare clears this.
    /// Cumulative `native_host_probes_performed` is audit evidence and does not
    /// latch this marker by itself.
    pub fn live_host_collection_verified(&self) -> bool {
        self.last_probe_kind == Some(HostSentinelProbeKind::NativeMacHost)
            && self.native_host_probes_performed > 0
            && self.violation.is_none()
            && self.last_probe_matches_baseline
    }

    /// Compare a live host probe to the baseline. Untyped callers are recorded as
    /// synthetic rehearsal so they cannot qualify a live collection claim.
    pub fn verify_via_probe(&mut self, observed: HostSentinelSnapshot) -> HarnessResult<()> {
        self.verify_via_probe_with_kind(observed, HostSentinelProbeKind::SyntheticRehearsal)
    }

    /// Run a host probe and compare to baseline.
    pub fn probe_and_verify(&mut self, probe: &dyn HostSentinelProbe) -> HarnessResult<()> {
        let kind = probe.probe_kind();
        match probe.probe_host() {
            Ok(observed) => self.verify_via_probe_with_kind(observed, kind),
            Err(err) => {
                self.record_unsuccessful_probe(kind);
                Err(err)
            }
        }
    }

    /// Compare an externally supplied snapshot to baseline.
    ///
    /// Does **not** count as native Mac collection — callers must use
    /// [`Self::refresh_from_native_collector`] for physical proof provenance.
    pub fn refresh_from_host(&mut self, snapshot: HostSentinelSnapshot) -> HarnessResult<()> {
        self.verify_via_probe_with_kind(snapshot, HostSentinelProbeKind::SyntheticRehearsal)
    }

    /// Native Mac physical proof hook: collect then compare to baseline.
    #[cfg(target_os = "macos")]
    pub fn refresh_from_native_collector(
        &mut self,
        collector: &MacHostSentinelCollector,
    ) -> HarnessResult<()> {
        match collector.collect() {
            Ok(snapshot) => {
                self.verify_via_probe_with_kind(snapshot, HostSentinelProbeKind::NativeMacHost)
            }
            Err(err) => {
                self.record_unsuccessful_probe(HostSentinelProbeKind::NativeMacHost);
                Err(err)
            }
        }
    }

    fn record_unsuccessful_probe(&mut self, kind: HostSentinelProbeKind) {
        self.last_probe_kind = Some(kind);
        self.last_probe_matches_baseline = false;
    }

    fn verify_via_probe_with_kind(
        &mut self,
        observed: HostSentinelSnapshot,
        kind: HostSentinelProbeKind,
    ) -> HarnessResult<()> {
        self.probes_performed = self.probes_performed.saturating_add(1);
        self.last_probe_kind = Some(kind);
        if observed != self.baseline {
            self.last_probe_matches_baseline = false;
            let detail = if observed.main_checkout_fence != self.baseline.main_checkout_fence {
                "main checkout fence drift detected by probe"
            } else {
                "host sentinel drift detected by probe"
            };
            self.violation = Some(detail.into());
            return Err(HarnessError::host_sentinel_violation(detail));
        }
        self.last_probe_matches_baseline = true;
        if kind == HostSentinelProbeKind::NativeMacHost {
            self.native_host_probes_performed = self.native_host_probes_performed.saturating_add(1);
        }
        Ok(())
    }

    pub fn verified_via_probe(&self) -> bool {
        self.probes_performed > 0 && self.violation.is_none() && self.last_probe_matches_baseline
    }

    pub fn assert_unchanged(&self) -> HarnessResult<()> {
        if self.probes_performed == 0 {
            return Err(HarnessError::host_sentinel_violation(
                "host sentinels were not verified via probe",
            ));
        }
        if let Some(message) = &self.violation {
            return Err(HarnessError::host_sentinel_violation(message.clone()));
        }
        if !self.last_probe_matches_baseline {
            return Err(HarnessError::host_sentinel_violation(
                "last host sentinel probe did not match baseline",
            ));
        }
        Ok(())
    }

    pub fn diff_report(&self, observed: &HostSentinelSnapshot) -> Option<HostSentinelDiff> {
        if &self.baseline == observed {
            return None;
        }
        Some(HostSentinelDiff {
            pointer_moved: self.baseline.pointer_x != observed.pointer_x
                || self.baseline.pointer_y != observed.pointer_y,
            foreground_changed: self.baseline.foreground_app_id != observed.foreground_app_id
                || self.baseline.foreground_window_id != observed.foreground_window_id,
            clipboard_changed: self.baseline.clipboard_digest != observed.clipboard_digest,
            unrelated_window_changed: self.baseline.unrelated_window_app_id
                != observed.unrelated_window_app_id
                || self.baseline.unrelated_window_id != observed.unrelated_window_id
                || self.baseline.unrelated_window_title_hash
                    != observed.unrelated_window_title_hash,
            main_checkout_changed: self.baseline.main_checkout_fence
                != observed.main_checkout_fence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelDiff {
    pub pointer_moved: bool,
    pub foreground_changed: bool,
    pub clipboard_changed: bool,
    pub unrelated_window_changed: bool,
    pub main_checkout_changed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assert_unchanged_fails_without_probe() {
        let registry = HostSentinelRegistry::capture(HostSentinelSnapshot::synthetic_baseline());
        let err = registry.assert_unchanged().expect_err("probe required");
        assert!(err.message.contains("not verified via probe"));
    }

    #[test]
    fn host_mutation_detected_by_probe() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let mut probe = SyntheticHostProbe::new(baseline.clone());
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&probe).expect("initial probe");

        probe.simulate_host_mutation("pointer");
        let err = registry.probe_and_verify(&probe).expect_err("mutation");
        assert_eq!(
            err.code,
            crate::error::HarnessErrorCode::HostSentinelViolation
        );
        assert!(!registry.verified_via_probe());
    }

    #[test]
    fn main_checkout_mutation_detected_by_probe() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let mut probe = SyntheticHostProbe::new(baseline.clone());
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&probe).expect("initial probe");

        probe.simulate_host_mutation("main_checkout");
        let err = registry.probe_and_verify(&probe).expect_err("mutation");
        assert_eq!(
            err.code,
            crate::error::HarnessErrorCode::HostSentinelViolation
        );
        assert!(err.message.contains("main checkout"));
    }

    #[test]
    fn synthetic_probe_kind_is_rehearsal_only() {
        let probe = SyntheticHostProbe::new(HostSentinelSnapshot::synthetic_baseline());
        assert_eq!(
            probe.probe_kind(),
            HostSentinelProbeKind::SyntheticRehearsal
        );
        assert_eq!(
            SyntheticHostProbe::PROBE_LABEL,
            "synthetic-rehearsal-only-not-physical-proof"
        );
    }

    #[test]
    fn refresh_from_host_does_not_count_as_native_collection() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let mut registry = HostSentinelRegistry::capture(baseline.clone());
        registry
            .refresh_from_host(baseline)
            .expect("synthetic refresh");
        assert_eq!(registry.native_host_probes_performed(), 0);
        assert!(!registry.live_host_collection_verified());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn mac_host_sentinel_unavailable_off_macos() {
        assert_eq!(
            crate::mac_host_sentinel::mac_host_sentinel_platform_support(),
            crate::mac_host_sentinel::MacHostSentinelPlatformSupport::UnavailableNonMacOs
        );
    }

    /// Test-only probe that reports `NativeMacHost` without binding a physical collector.
    struct NativeMacHostTestProbe {
        state: HostSentinelSnapshot,
    }

    impl HostSentinelProbe for NativeMacHostTestProbe {
        fn probe_host(&self) -> HarnessResult<HostSentinelSnapshot> {
            Ok(self.state.clone())
        }

        fn probe_kind(&self) -> HostSentinelProbeKind {
            HostSentinelProbeKind::NativeMacHost
        }
    }

    #[test]
    fn latest_successful_native_match_verifies_live_collection() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let native = NativeMacHostTestProbe {
            state: baseline.clone(),
        };
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&native).expect("native match");
        assert_eq!(registry.native_host_probes_performed(), 1);
        assert_eq!(
            registry.last_probe_kind,
            Some(HostSentinelProbeKind::NativeMacHost)
        );
        assert!(registry.live_host_collection_verified());
    }

    #[test]
    fn native_match_then_synthetic_match_clears_live_marker_and_keeps_native_count() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let native = NativeMacHostTestProbe {
            state: baseline.clone(),
        };
        let synthetic = SyntheticHostProbe::new(baseline.clone());
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&native).expect("native match");
        assert!(registry.live_host_collection_verified());

        registry
            .probe_and_verify(&synthetic)
            .expect("synthetic match");
        assert_eq!(registry.native_host_probes_performed(), 1);
        assert_eq!(
            registry.last_probe_kind,
            Some(HostSentinelProbeKind::SyntheticRehearsal)
        );
        assert!(!registry.live_host_collection_verified());
    }

    #[test]
    fn native_drift_never_verifies_live_collection() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let mut drifted = baseline.clone();
        drifted.pointer_x += 1;
        let matching = NativeMacHostTestProbe {
            state: baseline.clone(),
        };
        let drifting = NativeMacHostTestProbe { state: drifted };
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&matching).expect("native match");
        assert!(registry.live_host_collection_verified());

        let err = registry
            .probe_and_verify(&drifting)
            .expect_err("native drift");
        assert_eq!(
            err.code,
            crate::error::HarnessErrorCode::HostSentinelViolation
        );
        assert_eq!(registry.native_host_probes_performed(), 1);
        assert!(!registry.live_host_collection_verified());
    }

    #[test]
    fn missing_last_probe_kind_deserializes_fail_closed() {
        let baseline = HostSentinelSnapshot::synthetic_baseline();
        let native = NativeMacHostTestProbe {
            state: baseline.clone(),
        };
        let mut registry = HostSentinelRegistry::capture(baseline);
        registry.probe_and_verify(&native).expect("native match");
        assert!(registry.live_host_collection_verified());

        let mut value = serde_json::to_value(&registry).expect("serialize registry");
        value
            .as_object_mut()
            .expect("registry object")
            .remove("lastProbeKind");
        let restored: HostSentinelRegistry =
            serde_json::from_value(value).expect("old snapshot without lastProbeKind");
        assert_eq!(restored.native_host_probes_performed(), 1);
        assert!(restored.last_probe_matches_baseline);
        assert_eq!(restored.last_probe_kind, None);
        assert!(!restored.live_host_collection_verified());
    }
}
