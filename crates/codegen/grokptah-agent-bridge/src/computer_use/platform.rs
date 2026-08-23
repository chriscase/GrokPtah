use std::collections::BTreeSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::types::{
    validate_id, ActionClass, ComputerError, ComputerErrorCode, ComputerResult, ComputerTarget,
    ObservationGeometry,
};
use super::{ComputerStore, ComputerUseService};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerPermission {
    ScreenRecording,
    Accessibility,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerPermissionStatus {
    Unsupported,
    Missing,
    PromptPending,
    Denied,
    Granted,
    Revoked,
    Restricted,
}

impl ComputerPermissionStatus {
    pub fn is_granted(self) -> bool {
        self == Self::Granted
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerPlatformStatus {
    pub platform_id: String,
    pub available: bool,
    pub minimum_os_version: Option<String>,
    pub screen_recording: ComputerPermissionStatus,
    pub accessibility: ComputerPermissionStatus,
    pub detail: Option<String>,
}

/// The first fail-closed reason why isolated visual Computer Use is not
/// available. This is a read-only host/package projection; it is never
/// authority to construct an isolated capability proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputerIsolatedVisualBlocker {
    UnsupportedPlatform,
    MinimumOs,
    FrameworkUnavailable,
    BackendNotPackaged,
    HelperEntitlementUnverified,
    GuestImageNotMeasured,
}

/// Redaction-safe availability facts for the selected disposable-VM
/// substrate. Reading this value must never request consent, launch a VM,
/// inspect a guest image, or mint Computer Use authority. The main process is
/// intentionally not required to carry the helper's virtualization authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerIsolatedVisualStatus {
    pub platform_id: String,
    pub available: bool,
    pub host_capable: bool,
    pub minimum_os_version: String,
    pub operating_system_supported: bool,
    pub virtualization_framework_available: bool,
    pub helper_virtualization_entitlement_verified: bool,
    pub backend_packaged: bool,
    pub guest_image_measured: bool,
    pub launch_attempted: bool,
    pub blocker: Option<ComputerIsolatedVisualBlocker>,
    pub detail: String,
}

impl ComputerIsolatedVisualStatus {
    pub(crate) fn read_only_probe(
        platform_supported: bool,
        operating_system_supported: bool,
        virtualization_framework_available: bool,
    ) -> Self {
        let host_capable =
            platform_supported && operating_system_supported && virtualization_framework_available;
        let blocker = if !platform_supported {
            ComputerIsolatedVisualBlocker::UnsupportedPlatform
        } else if !operating_system_supported {
            ComputerIsolatedVisualBlocker::MinimumOs
        } else if !virtualization_framework_available {
            ComputerIsolatedVisualBlocker::FrameworkUnavailable
        } else {
            ComputerIsolatedVisualBlocker::BackendNotPackaged
        };
        let detail = match blocker {
            ComputerIsolatedVisualBlocker::UnsupportedPlatform => {
                "The selected isolated visual substrate currently requires macOS".into()
            }
            ComputerIsolatedVisualBlocker::MinimumOs => {
                "macOS 14 or later is required for the selected isolated visual substrate".into()
            }
            ComputerIsolatedVisualBlocker::FrameworkUnavailable => {
                "The required Apple Virtualization framework classes are unavailable".into()
            }
            ComputerIsolatedVisualBlocker::BackendNotPackaged => {
                "Host virtualization is ready; the signed helper and measured guest are not packaged yet"
                    .into()
            }
            ComputerIsolatedVisualBlocker::HelperEntitlementUnverified => unreachable!(
                "the read-only host probe never claims that a helper is packaged"
            ),
            ComputerIsolatedVisualBlocker::GuestImageNotMeasured => unreachable!(
                "the read-only substrate probe never claims that a backend is packaged"
            ),
        };
        Self {
            platform_id: if platform_supported {
                "macos"
            } else {
                "unavailable"
            }
            .into(),
            available: false,
            host_capable,
            minimum_os_version: "14.0".into(),
            operating_system_supported,
            virtualization_framework_available,
            helper_virtualization_entitlement_verified: false,
            backend_packaged: false,
            guest_image_measured: false,
            launch_attempted: false,
            blocker: Some(blocker),
            detail,
        }
    }

    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("platform_id", &self.platform_id)?;
        if self.minimum_os_version.is_empty()
            || self.minimum_os_version.len() > 64
            || self.detail.is_empty()
            || self.detail.len() > 512
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid isolated visual availability status",
            ));
        }
        let expected_host_capable = self.platform_id == "macos"
            && self.operating_system_supported
            && self.virtualization_framework_available;
        let expected_blocker = if self.platform_id == "unavailable" {
            ComputerIsolatedVisualBlocker::UnsupportedPlatform
        } else if self.platform_id != "macos" {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "isolated visual probe names an unsupported platform",
            ));
        } else if !self.operating_system_supported {
            ComputerIsolatedVisualBlocker::MinimumOs
        } else if !self.virtualization_framework_available {
            ComputerIsolatedVisualBlocker::FrameworkUnavailable
        } else {
            ComputerIsolatedVisualBlocker::BackendNotPackaged
        };
        if self.host_capable != expected_host_capable
            || (self.platform_id == "unavailable"
                && (self.operating_system_supported || self.virtualization_framework_available))
            || self.launch_attempted
            || self.available
            || self.backend_packaged
            || self.guest_image_measured
            || self.helper_virtualization_entitlement_verified
            || self.blocker != Some(expected_blocker)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "read-only isolated visual probe contains contradictory readiness claims",
            ));
        }
        Ok(())
    }
}

/// Read the selected isolated-visual substrate facts without launching or
/// configuring a virtual machine and without requesting any permission.
pub fn computer_isolated_visual_status() -> ComputerIsolatedVisualStatus {
    #[cfg(target_os = "macos")]
    {
        super::macos_native::isolated_visual_status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ComputerIsolatedVisualStatus::read_only_probe(false, false, false)
    }
}

impl ComputerPlatformStatus {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("platform_id", &self.platform_id)?;
        if self
            .minimum_os_version
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 64)
            || self
                .detail
                .as_ref()
                .is_some_and(|value| value.is_empty() || value.len() > 512)
        {
            return Err(ComputerError::new(
                ComputerErrorCode::InvalidRequest,
                "invalid computer-use platform status",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTargetCandidate {
    /// One-use, short-lived token issued by the local platform picker/list.
    pub selection_token: String,
    pub target: ComputerTarget,
    pub geometry: ObservationGeometry,
    pub on_screen: bool,
    pub active: bool,
    pub minimized: bool,
}

/// Short-lived local proof that one exact disposable target/action survived
/// reversible host measurement without changing the user's foreground app,
/// active window, or physical pointer. Native handles and element content are
/// deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerBackgroundSafetyReceipt {
    pub measurement_token: String,
    pub target: ComputerTarget,
    pub supported_action_classes: BTreeSet<ActionClass>,
    pub valid_for_millis: u64,
}

impl ComputerTargetCandidate {
    pub fn validate(&self) -> ComputerResult<()> {
        validate_id("selection_token", &self.selection_token)?;
        self.target.validate()?;
        self.geometry.validate()?;
        if self.target.sensitivity.is_hard_denied() {
            return Err(ComputerError::new(
                ComputerErrorCode::SensitiveSurface,
                "hard-denied target cannot be selected",
            ));
        }
        Ok(())
    }
}

#[async_trait]
pub trait ComputerObservationPlatform: Send + Sync + std::fmt::Debug {
    /// This must never trigger an operating-system permission prompt.
    fn status(&self) -> ComputerPlatformStatus;

    /// Call only from an explicit local-user permission action.
    async fn request_permission(
        &self,
        permission: ComputerPermission,
    ) -> ComputerResult<ComputerPermissionStatus>;

    /// Returns bounded local-user choices. It must not capture any target.
    async fn list_targets(&self) -> ComputerResult<Vec<ComputerTargetCandidate>>;

    /// Consumes a picker-issued token and returns a service around the bound
    /// backend. External platform implementations can construct only an
    /// Unproven service (or the simulator); trusted native bindings remain
    /// host-owned inside this crate.
    async fn bind_target_service(
        &self,
        selection_token: &str,
        store: ComputerStore,
    ) -> ComputerResult<ComputerUseService>;

    /// Explicit local-only calibration path. Implementations must mutate only
    /// an acknowledged disposable target and restore its original value.
    async fn measure_background_text_entry(
        &self,
        _selection_token: &str,
        _element_label: &str,
        _probe_text: &str,
        _disposable_target_acknowledged: bool,
    ) -> ComputerResult<ComputerBackgroundSafetyReceipt> {
        Err(ComputerError::new(
            ComputerErrorCode::UnsupportedPlatform,
            "this Computer platform does not support measured background calibration",
        ))
    }

    /// Consume one short-lived measurement and its exact picker selection.
    async fn bind_measured_background_target_service(
        &self,
        _selection_token: &str,
        _measurement_token: &str,
        _store: ComputerStore,
    ) -> ComputerResult<ComputerUseService> {
        Err(ComputerError::new(
            ComputerErrorCode::UnsupportedPlatform,
            "this Computer platform cannot bind measured background execution",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use::Sensitivity;

    #[test]
    fn hard_denied_candidate_is_invalid() {
        let candidate = ComputerTargetCandidate {
            selection_token: "selection".into(),
            target: ComputerTarget {
                app_id: "com.apple.loginwindow".into(),
                window_id: "window-1".into(),
                generation: 1,
                display_name: "Login Window".into(),
                sensitivity: Sensitivity::SystemRestricted,
            },
            geometry: ObservationGeometry {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
                scale_factor: 1.0,
            },
            on_screen: true,
            active: true,
            minimized: false,
        };
        assert_eq!(
            candidate.validate().unwrap_err().code,
            ComputerErrorCode::SensitiveSurface
        );
    }

    #[test]
    fn isolated_visual_probe_reports_the_first_exact_blocker_without_launching() {
        let cases = [
            (
                ComputerIsolatedVisualStatus::read_only_probe(false, false, false),
                ComputerIsolatedVisualBlocker::UnsupportedPlatform,
            ),
            (
                ComputerIsolatedVisualStatus::read_only_probe(true, false, false),
                ComputerIsolatedVisualBlocker::MinimumOs,
            ),
            (
                ComputerIsolatedVisualStatus::read_only_probe(true, true, false),
                ComputerIsolatedVisualBlocker::FrameworkUnavailable,
            ),
            (
                ComputerIsolatedVisualStatus::read_only_probe(true, true, true),
                ComputerIsolatedVisualBlocker::BackendNotPackaged,
            ),
        ];
        for (status, blocker) in cases {
            status.validate().unwrap();
            assert!(!status.available);
            assert!(!status.backend_packaged);
            assert!(!status.guest_image_measured);
            assert!(!status.launch_attempted);
            assert_eq!(status.blocker, Some(blocker));
        }
    }

    #[test]
    fn isolated_visual_probe_rejects_claim_upgrades() {
        let mut status = ComputerIsolatedVisualStatus::read_only_probe(true, true, true);
        assert!(status.host_capable);
        status.available = true;
        assert_eq!(
            status.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        status.available = false;
        status.launch_attempted = true;
        assert_eq!(
            status.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
        status.launch_attempted = false;
        status.helper_virtualization_entitlement_verified = true;
        assert_eq!(
            status.validate().unwrap_err().code,
            ComputerErrorCode::InvalidRequest
        );
    }

    #[test]
    fn runtime_isolated_visual_probe_is_read_only_and_valid() {
        let status = computer_isolated_visual_status();
        status.validate().unwrap();
        assert!(!status.available);
        assert!(!status.backend_packaged);
        assert!(!status.guest_image_measured);
        assert!(!status.launch_attempted);
    }
}
