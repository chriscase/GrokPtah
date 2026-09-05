//! Host sentinel snapshots for noninterference proof (#288/#286).
//!
//! The synthetic harness records pointer, foreground, clipboard, and unrelated
//! window state at harness start. **Unchanged** is only claimable after at least
//! one real host probe compares observed state to the baseline — never a no-op
//! self-compare of an uninitialized `current` field.

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, HarnessResult};

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
        }
    }
}

/// Reads live host sentinel state. Native Mac adapters implement this with real
/// AX/CGEvent/clipboard evidence; the synthetic harness uses [`SyntheticHostProbe`].
pub trait HostSentinelProbe {
    fn probe_host(&self) -> HarnessResult<HostSentinelSnapshot>;
}

/// Synthetic host-side state for harness tests. Starts at the baseline and may
/// only diverge when a test simulates host mutation.
#[derive(Debug, Clone)]
pub struct SyntheticHostProbe {
    state: HostSentinelSnapshot,
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
}

/// Tracks host sentinel drift during a harness run. Guest operations must never
/// mutate host sentinels — violations fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelRegistry {
    baseline: HostSentinelSnapshot,
    violation: Option<String>,
    probes_performed: u32,
    last_probe_matches_baseline: bool,
}

impl HostSentinelRegistry {
    pub fn capture(baseline: HostSentinelSnapshot) -> Self {
        Self {
            baseline,
            violation: None,
            probes_performed: 0,
            last_probe_matches_baseline: false,
        }
    }

    pub fn baseline(&self) -> &HostSentinelSnapshot {
        &self.baseline
    }

    pub fn probes_performed(&self) -> u32 {
        self.probes_performed
    }

    /// Compare a live host probe to the baseline. This is the only path that may
    /// authorize an unchanged claim.
    pub fn verify_via_probe(&mut self, observed: HostSentinelSnapshot) -> HarnessResult<()> {
        self.probes_performed = self.probes_performed.saturating_add(1);
        if observed != self.baseline {
            self.last_probe_matches_baseline = false;
            self.violation = Some("host sentinel drift detected by probe".into());
            return Err(HarnessError::host_sentinel_violation(
                "host sentinel probe differs from baseline",
            ));
        }
        self.last_probe_matches_baseline = true;
        Ok(())
    }

    /// Run a host probe and compare to baseline.
    pub fn probe_and_verify(&mut self, probe: &dyn HostSentinelProbe) -> HarnessResult<()> {
        let observed = probe.probe_host()?;
        self.verify_via_probe(observed)
    }

    /// Extension point for native Mac evidence collectors that already performed
    /// an external read — still requires comparison to baseline.
    pub fn refresh_from_host(&mut self, snapshot: HostSentinelSnapshot) -> HarnessResult<()> {
        self.verify_via_probe(snapshot)
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
}
