use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use base64::Engine;
use chrono::{Duration, Utc};
use grokptah_agent_bridge::{
    grokptah_home, ActionClass, ActionGrant, ComputerObservation, ComputerObservationPlatform,
    ComputerPermission, ComputerPermissionStatus, ComputerPlatformStatus, ComputerStore,
    ComputerTargetCandidate, ComputerUseLimits, ComputerUseService, GrantIssuer,
    MacOsObservationPlatform,
};
use serde::Serialize;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservationPreview {
    pub observation: ComputerObservation,
    pub image_data_url: Option<String>,
}

pub struct DesktopComputerUse {
    platform: Option<Arc<dyn ComputerObservationPlatform>>,
    store: Option<ComputerStore>,
    initialization_error: Option<String>,
    operation: Mutex<()>,
    selections: std::sync::Mutex<HashMap<String, grokptah_agent_bridge::ComputerTarget>>,
}

impl DesktopComputerUse {
    pub fn new() -> Self {
        let (platform, platform_error) = native_platform();
        let (store, store_error) = match ComputerStore::open(grokptah_home().join("computer-use")) {
            Ok(store) => (Some(store), None),
            Err(error) => (
                None,
                Some(format!("Computer Use storage is unavailable: {error}")),
            ),
        };
        Self {
            platform,
            store,
            initialization_error: platform_error.or(store_error),
            operation: Mutex::new(()),
            selections: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn status(&self) -> ComputerPlatformStatus {
        match &self.platform {
            Some(platform) => {
                let mut status = platform.status();
                if let Some(error) = &self.initialization_error {
                    status.available = false;
                    status.detail = Some(error.clone());
                }
                status
            }
            None => unsupported_status(self.initialization_error.clone()),
        }
    }

    pub async fn request_permission(
        &self,
        permission: ComputerPermission,
    ) -> Result<ComputerPermissionStatus, String> {
        let _guard = self.operation.lock().await;
        let platform = self.platform()?;
        platform
            .request_permission(permission)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn list_targets(&self) -> Result<Vec<ComputerTargetCandidate>, String> {
        let _guard = self.operation.lock().await;
        self.selections
            .lock()
            .map_err(|_| "Computer Use selection state is unavailable".to_string())?
            .clear();
        let targets = self
            .platform()?
            .list_targets()
            .await
            .map_err(|error| error.to_string())?;
        let mut selections = self
            .selections
            .lock()
            .map_err(|_| "Computer Use selection state is unavailable".to_string())?;
        selections.extend(
            targets
                .iter()
                .map(|candidate| (candidate.selection_token.clone(), candidate.target.clone())),
        );
        Ok(targets)
    }

    /// Performs one explicitly requested, read-only observation and then
    /// destroys the backend evidence. No action API is exposed to Tauri.
    pub async fn observe_once(
        &self,
        selection_token: &str,
        owner_session_id: Uuid,
    ) -> Result<ObservationPreview, String> {
        let _guard = self.operation.lock().await;
        let platform = self.platform()?;
        let store = self
            .store
            .clone()
            .ok_or_else(|| self.initialization_error())?;
        let target = self
            .selections
            .lock()
            .map_err(|_| "Computer Use selection state is unavailable".to_string())?
            .remove(selection_token)
            .ok_or_else(|| {
                "Computer Use selection is stale; refresh the window list".to_string()
            })?;
        let backend = platform
            .bind_target(selection_token)
            .await
            .map_err(|error| error.to_string())?;
        let service = ComputerUseService::new(backend, store);
        let limits = ComputerUseLimits {
            max_actions: 1,
            max_duration_secs: 5 * 60,
            max_screenshot_dimension: 4096,
            max_evidence_bytes: 8 * 1024 * 1024,
            ..ComputerUseLimits::default()
        };
        let run = service
            .create_run(
                &Uuid::new_v4().to_string(),
                owner_session_id,
                target,
                limits,
            )
            .map_err(|error| error.to_string())?;
        let now = Utc::now();
        let grant = ActionGrant {
            grant_id: Uuid::new_v4().to_string(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic]),
            issued_by: GrantIssuer::LocalUser,
            issued_at: now,
            expires_at: now + Duration::minutes(5),
            uses_remaining: Some(1),
            revoked_at: None,
        };
        let run =
            match service.authorize(&Uuid::new_v4().to_string(), &run.run_id, run.version, grant) {
                Ok(run) => run,
                Err(error) => {
                    let _ = service
                        .cancel(&Uuid::new_v4().to_string(), &run.run_id)
                        .await;
                    return Err(error.to_string());
                }
            };
        let observed = service
            .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
            .await;
        let preview = match observed {
            Ok(observation) => {
                let image_data_url = match observation.screenshot.as_ref() {
                    Some(evidence) => service
                        .read_current_evidence(&run.run_id, &evidence.asset_id)
                        .await
                        .map(|bytes| {
                            Some(format!(
                                "data:image/png;base64,{}",
                                base64::engine::general_purpose::STANDARD.encode(bytes)
                            ))
                        })
                        .map_err(|error| error.to_string()),
                    None => Ok(None),
                };
                image_data_url.map(|image_data_url| ObservationPreview {
                    observation,
                    image_data_url,
                })
            }
            Err(error) => Err(error.to_string()),
        };
        let cleanup = service
            .cancel(&Uuid::new_v4().to_string(), &run.run_id)
            .await
            .map_err(|error| error.to_string());
        match (preview, cleanup) {
            (Ok(preview), Ok(_)) => Ok(preview),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(format!("Computer Use cleanup failed: {error}")),
        }
    }

    fn platform(&self) -> Result<Arc<dyn ComputerObservationPlatform>, String> {
        self.platform
            .clone()
            .ok_or_else(|| self.initialization_error())
    }

    fn initialization_error(&self) -> String {
        self.initialization_error
            .clone()
            .unwrap_or_else(|| "Computer Use is unavailable on this platform".into())
    }
}

#[cfg(target_os = "macos")]
fn native_platform() -> (Option<Arc<dyn ComputerObservationPlatform>>, Option<String>) {
    match MacOsObservationPlatform::new_native() {
        Ok(platform) => (Some(Arc::new(platform)), None),
        Err(error) => (None, Some(error.to_string())),
    }
}

#[cfg(not(target_os = "macos"))]
fn native_platform() -> (Option<Arc<dyn ComputerObservationPlatform>>, Option<String>) {
    (
        None,
        Some("The native Computer Use adapter is currently available on macOS".into()),
    )
}

fn unsupported_status(detail: Option<String>) -> ComputerPlatformStatus {
    ComputerPlatformStatus {
        platform_id: "unavailable".into(),
        available: false,
        minimum_os_version: None,
        screen_recording: ComputerPermissionStatus::Unsupported,
        accessibility: ComputerPermissionStatus::Unsupported,
        detail,
    }
}
