use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::package_identity::ComputerExecutorIdentity;
use super::types::{
    validate_id, ComputerBackend, ComputerError, ComputerErrorCode, ComputerResult, ComputerTarget,
    ObservationGeometry,
};

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
    /// Which code identity TCC would attach to for this platform.
    ///
    /// A status that reports a usable platform answers this, because that is
    /// exactly the moment a reader could otherwise assume a packaged helper.
    /// In-process host identity is never packaged-helper qualification, and
    /// saying so positively is the point of the field.
    ///
    /// `None` means there is no usable platform and therefore no executor at
    /// all. It does not mean "unknown, possibly packaged". Nothing derives
    /// readiness from this field: packaged admission is computed from the
    /// preflight in `grokptah-isolated-visual`, never from a status a platform
    /// reports about itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor: Option<ComputerExecutorIdentity>,
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
        if let Some(executor) = &self.executor {
            executor.validate()?;
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

    /// Consumes a picker-issued token so arbitrary native IDs cannot be bound.
    async fn bind_target(&self, selection_token: &str) -> ComputerResult<Arc<dyn ComputerBackend>>;
}

#[cfg(test)]
mod tests {
    use super::super::package_identity::{ExecutorKind, SigningClass};
    use super::*;
    use crate::computer_use::Sensitivity;

    fn status_with(executor: Option<ComputerExecutorIdentity>) -> ComputerPlatformStatus {
        ComputerPlatformStatus {
            platform_id: "macos".into(),
            available: executor.is_some(),
            minimum_os_version: Some("14.0".into()),
            screen_recording: ComputerPermissionStatus::Granted,
            accessibility: ComputerPermissionStatus::Granted,
            detail: None,
            executor,
        }
    }

    /// The in-process host identity must validate inside a platform status.
    ///
    /// `MacOsObservationPlatform::status` falls back to a Restricted status
    /// whenever the native source's status fails `validate()`, so an identity
    /// that did not validate would silently disable macOS Computer Use. That
    /// path is `cfg(target_os = "macos")` and does not compile on Linux, so
    /// this test pins the identity itself on every platform.
    #[test]
    fn in_process_host_executor_validates_inside_a_platform_status() {
        let identity = ComputerExecutorIdentity::in_process_host(SigningClass::Uninspected);
        status_with(Some(identity.clone())).validate().unwrap();
        assert_eq!(identity.kind, ExecutorKind::InProcessHost);
        // Being present in a granted status is never packaged qualification.
        assert!(!identity.signing_class.counts_as_packaged_release());
        assert!(identity.team_id.is_none());
        assert!(identity.designated_requirement.is_none());
    }

    /// `None` is a real answer ("no usable platform"), not a missing one.
    #[test]
    fn a_status_without_a_platform_reports_no_executor() {
        let mut status = status_with(None);
        status.available = false;
        status.screen_recording = ComputerPermissionStatus::Unsupported;
        status.accessibility = ComputerPermissionStatus::Unsupported;
        status.validate().unwrap();
        assert!(status.executor.is_none());
        // Absent on the wire, so no consumer sees a null executor object.
        let encoded = serde_json::to_value(&status).unwrap();
        assert!(encoded.get("executor").is_none());
    }

    /// A status must not be able to carry a packaged-helper claim that the
    /// identity contract itself would reject.
    #[test]
    fn a_status_cannot_smuggle_an_unbacked_packaged_identity() {
        let mut forged = ComputerExecutorIdentity::in_process_host(SigningClass::Uninspected);
        forged.team_id = Some("TEAMID1234".into());
        forged.designated_requirement = Some("identifier \"anything\"".into());
        assert_eq!(
            status_with(Some(forged)).validate().unwrap_err().code,
            ComputerErrorCode::Unauthorized
        );
    }

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
}
