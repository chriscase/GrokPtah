//! Host sentinel snapshots for noninterference proof (#288/#286).
//!
//! The synthetic harness records pointer, foreground, clipboard, and unrelated
//! window state at harness start and exposes assertions that no guest lifecycle
//! operation mutated them. Native Mac adapters implement the same trait surface
//! with real AX/CGEvent evidence later — never inferred from Linux CI.

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

/// Tracks host sentinel drift during a harness run. Guest operations must never
/// mutate host sentinels — violations fail closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostSentinelRegistry {
    baseline: HostSentinelSnapshot,
    current: HostSentinelSnapshot,
    violation: Option<String>,
}

impl HostSentinelRegistry {
    pub fn capture(baseline: HostSentinelSnapshot) -> Self {
        Self {
            baseline: baseline.clone(),
            current: baseline,
            violation: None,
        }
    }

    pub fn baseline(&self) -> &HostSentinelSnapshot {
        &self.baseline
    }

    pub fn current(&self) -> &HostSentinelSnapshot {
        &self.current
    }

    /// Extension point for native Mac evidence collectors.
    pub fn refresh_from_host(&mut self, snapshot: HostSentinelSnapshot) {
        self.current = snapshot;
        if self.current != self.baseline {
            self.violation = Some("host sentinel drift detected on refresh".into());
        }
    }

    /// Synthetic guest backends call this to prove they did not touch host state.
    pub fn record_guest_side_effect(&mut self, field: &str) {
        self.violation = Some(format!("guest attempted host mutation: {field}"));
    }

    pub fn assert_unchanged(&self) -> HarnessResult<()> {
        if let Some(message) = &self.violation {
            return Err(HarnessError::host_sentinel_violation(message.clone()));
        }
        if self.current != self.baseline {
            return Err(HarnessError::host_sentinel_violation(
                "host sentinel snapshot differs from baseline",
            ));
        }
        Ok(())
    }

    pub fn diff_report(&self) -> Option<HostSentinelDiff> {
        if self.baseline == self.current {
            return None;
        }
        Some(HostSentinelDiff {
            pointer_moved: self.baseline.pointer_x != self.current.pointer_x
                || self.baseline.pointer_y != self.current.pointer_y,
            foreground_changed: self.baseline.foreground_app_id != self.current.foreground_app_id
                || self.baseline.foreground_window_id != self.current.foreground_window_id,
            clipboard_changed: self.baseline.clipboard_digest != self.current.clipboard_digest,
            unrelated_window_changed: self.baseline.unrelated_window_app_id
                != self.current.unrelated_window_app_id
                || self.baseline.unrelated_window_id != self.current.unrelated_window_id
                || self.baseline.unrelated_window_title_hash
                    != self.current.unrelated_window_title_hash,
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
