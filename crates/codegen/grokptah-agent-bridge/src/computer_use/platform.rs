use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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
}
