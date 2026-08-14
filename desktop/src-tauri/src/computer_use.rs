use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use base64::Engine;
use chrono::{Duration, Utc};
use grokptah_agent_bridge::{
    grokptah_home, ActionClass, ActionGrant, ComputerAction, ComputerCapabilities, ComputerError,
    ComputerObservation, ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerRun, ComputerRunState, ComputerStore, ComputerTargetCandidate,
    ComputerUseLimits, ComputerUseService, GrantIssuer, MacOsObservationPlatform, SemanticAction,
    SimulatorBackend,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingComputerApproval {
    pub approval_id: String,
    pub owner_session_id: Uuid,
    pub run_id: String,
    pub run_version: u64,
    pub observation_id: String,
    pub target_label: String,
    pub action: ComputerAction,
    pub action_summary: String,
    pub risk: String,
    pub created_at: chrono::DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerCockpitSnapshot {
    pub backend: ComputerCapabilities,
    pub origin: String,
    pub run: Option<ComputerRun>,
    pub pending_approval: Option<PendingComputerApproval>,
}

pub struct DesktopComputerUse {
    platform: Option<Arc<dyn ComputerObservationPlatform>>,
    store: Option<ComputerStore>,
    initialization_error: Option<String>,
    operation: Mutex<()>,
    selections: std::sync::Mutex<HashMap<String, grokptah_agent_bridge::ComputerTarget>>,
    simulator: Option<ComputerUseService>,
    simulator_operation: Mutex<()>,
    pending_approval: std::sync::Mutex<Option<PendingComputerApproval>>,
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
        let simulator = store
            .clone()
            .map(|store| ComputerUseService::new(Arc::new(SimulatorBackend::new()), store));
        Self {
            platform,
            store,
            initialization_error: platform_error.or(store_error),
            operation: Mutex::new(()),
            selections: std::sync::Mutex::new(HashMap::new()),
            simulator,
            simulator_operation: Mutex::new(()),
            pending_approval: std::sync::Mutex::new(None),
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

    pub fn cockpit_snapshot(
        &self,
        owner_session_id: Uuid,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let service = self.simulator()?;
        let run = latest_simulator_run(service, owner_session_id)?;
        let mut pending = self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?;
        if pending
            .as_ref()
            .is_some_and(|approval| approval.owner_session_id != owner_session_id)
        {
            *pending = None;
        }
        let pending_approval = pending
            .clone()
            .filter(|pending| run.as_ref().is_some_and(|run| run.run_id == pending.run_id));
        Ok(ComputerCockpitSnapshot {
            backend: service.capabilities(),
            origin: "desktop".into(),
            run,
            pending_approval,
        })
    }

    pub async fn start_simulator(
        &self,
        owner_session_id: Uuid,
        reviewed_target_app_id: &str,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _guard = self.simulator_operation.lock().await;
        let service = self.simulator()?;
        let target = SimulatorBackend::demo_target();
        if reviewed_target_app_id != target.app_id {
            return Err("The reviewed Computer Use target no longer matches".into());
        }
        if has_active_simulator_run(service, owner_session_id)? {
            return Err("This session already has an active Computer Run".into());
        }
        self.clear_pending_for_owner(owner_session_id)?;
        let limits = ComputerUseLimits {
            max_actions: 8,
            max_duration_secs: 10 * 60,
            max_observation_age_millis: 30_000,
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
        if let Err(error) = authorize_and_observe_once(service, &run).await {
            let _ = service
                .cancel(&Uuid::new_v4().to_string(), &run.run_id)
                .await;
            return Err(error.to_string());
        }
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn refresh_simulator(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _guard = self.simulator_operation.lock().await;
        let service = self.simulator()?;
        let run = owned_run(service, owner_session_id, run_id)?;
        if run.version != expected_version {
            return Err(format!(
                "Stale Computer Run version: expected {expected_version}, current {}",
                run.version
            ));
        }
        if run.state != ComputerRunState::Paused {
            return Err("Only a paused Computer Run can be reauthorized".into());
        }
        self.clear_pending_for_owner(owner_session_id)?;
        authorize_and_observe_once(service, &run)
            .await
            .map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn stage_simulator_action(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _guard = self.simulator_operation.lock().await;
        let service = self.simulator()?;
        let run = owned_run(service, owner_session_id, run_id)?;
        if run.version != expected_version {
            return Err("The proposed action is based on a stale Computer Run".into());
        }
        if run.state != ComputerRunState::Ready {
            return Err("The Computer Run is not ready for an action".into());
        }
        let observation = run
            .current_observation
            .as_ref()
            .filter(|observation| observation.observation_id == observation_id)
            .ok_or_else(|| "The proposed action is based on a stale observation".to_string())?;
        action
            .validate(&run.limits)
            .map_err(|error| error.to_string())?;

        let (action_summary, risk, required_semantic_action) = match &action {
            ComputerAction::SetValue { .. } => (
                "Enter visible text in Name".to_string(),
                "Text entry".to_string(),
                SemanticAction::SetValue,
            ),
            ComputerAction::Invoke { .. } => (
                "Invoke Submit".to_string(),
                "Semantic action".to_string(),
                SemanticAction::Invoke,
            ),
            _ => return Err("The simulator cockpit accepts semantic form actions only".into()),
        };
        let element_id = action
            .referenced_element()
            .ok_or_else(|| "The proposed action does not identify an element".to_string())?;
        let element = observation
            .element(element_id)
            .filter(|element| element.enabled)
            .ok_or_else(|| "The proposed element is missing or disabled".to_string())?;
        if element.sensitivity.is_hard_denied()
            || !element.actions.contains(&required_semantic_action)
        {
            return Err("The proposed semantic action is outside the observed scope".into());
        }

        let mut pending = self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?;
        if pending.is_some() {
            return Err("Resolve or discard the current Computer Use approval first".into());
        }
        *pending = Some(PendingComputerApproval {
            approval_id: Uuid::new_v4().to_string(),
            owner_session_id,
            run_id: run.run_id,
            run_version: run.version,
            observation_id: observation.observation_id.clone(),
            target_label: observation.target.display_name.clone(),
            action,
            action_summary,
            risk,
            created_at: Utc::now(),
        });
        drop(pending);
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn approve_simulator_action(
        &self,
        owner_session_id: Uuid,
        approval_id: &str,
        request_id: &str,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _guard = self.simulator_operation.lock().await;
        let service = self.simulator()?;
        let pending = self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?
            .clone()
            .filter(|pending| {
                pending.owner_session_id == owner_session_id && pending.approval_id == approval_id
            })
            .ok_or_else(|| "This Computer Use approval is stale or already resolved".to_string())?;
        let run = owned_run(service, owner_session_id, &pending.run_id)?;
        if run.version != pending.run_version
            || run
                .current_observation
                .as_ref()
                .map(|observation| observation.observation_id.as_str())
                != Some(pending.observation_id.as_str())
        {
            self.clear_pending_for_owner(owner_session_id)?;
            return Err("This Computer Use approval no longer matches the live run".into());
        }
        let result = service
            .act(
                request_id,
                &pending.run_id,
                pending.run_version,
                &pending.observation_id,
                pending.action,
            )
            .await;
        self.clear_pending_for_owner(owner_session_id)?;
        result.map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    pub fn discard_simulator_approval(
        &self,
        owner_session_id: Uuid,
    ) -> Result<ComputerCockpitSnapshot, String> {
        self.clear_pending_for_owner(owner_session_id)?;
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn pause_simulator(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerCockpitSnapshot, String> {
        self.clear_pending_for_owner(owner_session_id)?;
        let service = self.simulator()?;
        owned_run(service, owner_session_id, run_id)?;
        service
            .pause(&Uuid::new_v4().to_string(), run_id, expected_version)
            .await
            .map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn take_over_simulator(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
    ) -> Result<ComputerCockpitSnapshot, String> {
        self.clear_pending_for_owner(owner_session_id)?;
        let service = self.simulator()?;
        owned_run(service, owner_session_id, run_id)?;
        service
            .take_over(&Uuid::new_v4().to_string(), run_id, expected_version)
            .await
            .map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn stop_simulator(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
    ) -> Result<ComputerCockpitSnapshot, String> {
        self.clear_pending_for_owner(owner_session_id)?;
        let service = self.simulator()?;
        owned_run(service, owner_session_id, run_id)?;
        service
            .cancel(&Uuid::new_v4().to_string(), run_id)
            .await
            .map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    fn simulator(&self) -> Result<&ComputerUseService, String> {
        self.simulator
            .as_ref()
            .ok_or_else(|| self.initialization_error())
    }

    fn clear_pending_for_owner(&self, owner_session_id: Uuid) -> Result<(), String> {
        let mut pending = self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?;
        if pending
            .as_ref()
            .is_some_and(|pending| pending.owner_session_id == owner_session_id)
        {
            *pending = None;
        }
        Ok(())
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

fn latest_simulator_run(
    service: &ComputerUseService,
    owner_session_id: Uuid,
) -> Result<Option<ComputerRun>, String> {
    service
        .list_runs()
        .map_err(|error| error.to_string())
        .map(|runs| {
            runs.into_iter()
                .filter(|run| {
                    run.owner_session_id == owner_session_id
                        && run.target.app_id == SimulatorBackend::demo_target().app_id
                })
                .max_by_key(|run| run.updated_at)
        })
}

fn has_active_simulator_run(
    service: &ComputerUseService,
    owner_session_id: Uuid,
) -> Result<bool, String> {
    service
        .list_runs()
        .map_err(|error| error.to_string())
        .map(|runs| {
            runs.into_iter().any(|run| {
                run.owner_session_id == owner_session_id
                    && run.target.app_id == SimulatorBackend::demo_target().app_id
                    && !run.state.is_terminal()
            })
        })
}

fn owned_run(
    service: &ComputerUseService,
    owner_session_id: Uuid,
    run_id: &str,
) -> Result<ComputerRun, String> {
    service
        .get_run(run_id)
        .map_err(|error| error.to_string())?
        .filter(|run| {
            run.owner_session_id == owner_session_id
                && run.target.app_id == SimulatorBackend::demo_target().app_id
        })
        .ok_or_else(|| "Computer Run does not belong to this session".to_string())
}

async fn authorize_and_observe_once(
    service: &ComputerUseService,
    run: &ComputerRun,
) -> Result<ComputerObservation, ComputerError> {
    let now = Utc::now();
    let grant = ActionGrant {
        grant_id: Uuid::new_v4().to_string(),
        run_id: run.run_id.clone(),
        target: run.target.clone(),
        action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
        issued_by: GrantIssuer::LocalUser,
        issued_at: now,
        expires_at: now + Duration::minutes(2),
        uses_remaining: Some(1),
        revoked_at: None,
    };
    let run = service.authorize(&Uuid::new_v4().to_string(), &run.run_id, run.version, grant)?;
    service
        .observe(&Uuid::new_v4().to_string(), &run.run_id, run.version)
        .await
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_desktop() -> (tempfile::TempDir, DesktopComputerUse) {
        let dir = tempfile::tempdir().unwrap();
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let simulator = ComputerUseService::new(Arc::new(SimulatorBackend::new()), store.clone());
        (
            dir,
            DesktopComputerUse {
                platform: None,
                store: Some(store),
                initialization_error: None,
                operation: Mutex::new(()),
                selections: std::sync::Mutex::new(HashMap::new()),
                simulator: Some(simulator),
                simulator_operation: Mutex::new(()),
                pending_approval: std::sync::Mutex::new(None),
            },
        )
    }

    #[tokio::test]
    async fn approval_is_exact_one_use_and_requires_reobservation() {
        let (_dir, desktop) = test_desktop();
        let owner = Uuid::new_v4();
        let target = SimulatorBackend::demo_target();
        let started = desktop
            .start_simulator(owner, &target.app_id)
            .await
            .unwrap();
        let run = started.run.unwrap();
        let observation = run.current_observation.as_ref().unwrap();
        let staged = desktop
            .stage_simulator_action(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Ada".into(),
                },
            )
            .await
            .unwrap();
        let approval = staged.pending_approval.unwrap();
        let acted = desktop
            .approve_simulator_action(owner, &approval.approval_id, "approve-once")
            .await
            .unwrap();
        let acted_run = acted.run.unwrap();
        assert_eq!(acted_run.action_count, 1);
        assert_eq!(acted_run.state, ComputerRunState::Paused);
        assert!(acted_run.current_observation.is_none());
        assert!(desktop
            .approve_simulator_action(owner, &approval.approval_id, "approve-once")
            .await
            .is_err());
        assert_eq!(
            desktop
                .cockpit_snapshot(owner)
                .unwrap()
                .run
                .unwrap()
                .action_count,
            1
        );

        let refreshed = desktop
            .refresh_simulator(owner, &acted_run.run_id, acted_run.version)
            .await
            .unwrap();
        assert!(refreshed.run.unwrap().current_observation.is_some());
    }

    #[tokio::test]
    async fn approval_cannot_cross_sessions_or_survive_takeover() {
        let (_dir, desktop) = test_desktop();
        let owner = Uuid::new_v4();
        let other = Uuid::new_v4();
        let target = SimulatorBackend::demo_target();
        let started = desktop
            .start_simulator(owner, &target.app_id)
            .await
            .unwrap();
        let run = started.run.unwrap();
        let observation = run.current_observation.as_ref().unwrap();
        let staged = desktop
            .stage_simulator_action(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::Invoke {
                    element_id: format!("{}-submit", observation.observation_id),
                },
            )
            .await;
        assert!(staged.is_err(), "disabled submit must not reach approval");

        let staged = desktop
            .stage_simulator_action(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: format!("{}-name", observation.observation_id),
                    text: "Grace".into(),
                },
            )
            .await
            .unwrap();
        let approval = staged.pending_approval.unwrap();
        assert!(desktop
            .approve_simulator_action(other, &approval.approval_id, "cross-session")
            .await
            .is_err());
        desktop.cockpit_snapshot(other).unwrap();
        assert!(desktop
            .approve_simulator_action(owner, &approval.approval_id, "after-session-switch")
            .await
            .is_err());
        let taken_over = desktop
            .take_over_simulator(owner, &run.run_id, run.version)
            .await
            .unwrap();
        assert_eq!(taken_over.run.unwrap().state, ComputerRunState::Paused);
        assert!(desktop
            .approve_simulator_action(owner, &approval.approval_id, "after-takeover")
            .await
            .is_err());
    }
}
