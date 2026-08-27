use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use base64::Engine;
use chrono::{Duration, Utc};
use grokptah_agent_bridge::{
    canonical_workspace_string, ActionClass, ActionGrant, AgentHostHandle, ComputerAction,
    ComputerAgentProposal, ComputerCapabilities, ComputerError, ComputerObservation,
    ComputerObservationPlatform, ComputerPermission, ComputerPermissionStatus,
    ComputerPlatformStatus, ComputerRun, ComputerRunProjection, ComputerRunState,
    ComputerTargetCandidate, ComputerUseLimits, ComputerUseService, GrantIssuer,
    MacOsObservationPlatform, SemanticAction, SimulatorBackend,
};
use serde::Serialize;
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_LIVE_NATIVE_SERVICES: usize = 32;

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
    /// Authoritative run view. This is the identical serialized projection a
    /// coordinator surface receives, so the cockpit and an external observer
    /// cannot disagree about state, control disposition, epoch, or progress.
    pub projection: Option<ComputerRunProjection>,
    /// Local-only detail. It carries observed element labels and values needed
    /// to render an approval and must never cross the MCP boundary; the
    /// projection above is what does.
    pub run: Option<ComputerRun>,
    pub pending_approval: Option<PendingComputerApproval>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerAgentProposalResult {
    pub snapshot: ComputerCockpitSnapshot,
    pub summary: String,
    pub completed: bool,
}

pub struct DesktopComputerUse {
    host: AgentHostHandle,
    platform: Option<Arc<dyn ComputerObservationPlatform>>,
    store: Option<grokptah_agent_bridge::ComputerStore>,
    initialization_error: Option<String>,
    operation: Mutex<()>,
    selections: std::sync::Mutex<HashMap<String, grokptah_agent_bridge::ComputerTarget>>,
    simulator: Option<Arc<ComputerUseService>>,
    native_services: std::sync::Mutex<HashMap<String, Arc<ComputerUseService>>>,
    simulator_operation: Mutex<()>,
    pending_approval: std::sync::Mutex<Option<PendingComputerApproval>>,
}

impl DesktopComputerUse {
    pub fn new(host: &AgentHostHandle) -> Self {
        let (platform, platform_error) = native_platform();
        // The durable ledger holds an exclusive file lock, so the desktop and
        // the embedded MCP control plane must share the host's single handle
        // rather than each opening their own store.
        let (store, store_error) = match host.ensure_computer_store() {
            Ok(store) => (Some(store), None),
            Err(error) => (
                None,
                Some(format!("Computer Use storage is unavailable: {error}")),
            ),
        };
        let simulator = store.clone().map(|store| {
            Arc::new(ComputerUseService::new(
                Arc::new(SimulatorBackend::new()),
                store,
            ))
        });
        Self {
            host: host.clone(),
            platform,
            store,
            initialization_error: platform_error.or(store_error),
            operation: Mutex::new(()),
            selections: std::sync::Mutex::new(HashMap::new()),
            simulator,
            native_services: std::sync::Mutex::new(HashMap::new()),
            simulator_operation: Mutex::new(()),
            pending_approval: std::sync::Mutex::new(None),
        }
    }

    /// Durable workspace binding for a new run: the owning session's canonical
    /// project cwd. `None` (no session, empty cwd, or a path that cannot be
    /// canonicalized) keeps the run fully usable from the desktop but
    /// invisible to workspace-scoped MCP reads, which fail closed on an
    /// absent binding rather than inferring one from process state.
    fn session_workspace(&self, owner_session_id: Uuid) -> Option<String> {
        let session = self.host.session_load(owner_session_id).ok()?;
        if session.cwd.is_empty() {
            return None;
        }
        canonical_workspace_string(std::path::Path::new(&session.cwd))
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
                self.session_workspace(owner_session_id),
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
        let index = self.simulator()?;
        let run = latest_desktop_run(&index, owner_session_id)?;
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
        let backend = match run.as_ref() {
            Some(run) if run.target.app_id == SimulatorBackend::demo_target().app_id => {
                index.capabilities()
            }
            Some(run) => self
                .native_services
                .lock()
                .map_err(|_| "Computer Use native run state is unavailable".to_string())?
                .get(&run.run_id)
                .map(|service| service.capabilities())
                .unwrap_or_else(unavailable_native_capabilities),
            None => index.capabilities(),
        };
        Ok(ComputerCockpitSnapshot {
            backend,
            origin: "desktop".into(),
            projection: run
                .as_ref()
                .map(|run| grokptah_agent_bridge::project_run_at(run, Utc::now())),
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
        if has_active_desktop_run(&service, owner_session_id)? {
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
                self.session_workspace(owner_session_id),
                target,
                limits,
            )
            .map_err(|error| error.to_string())?;
        if let Err(error) = authorize_and_observe_once(&service, &run).await {
            let _ = service
                .cancel(&Uuid::new_v4().to_string(), &run.run_id)
                .await;
            return Err(error.to_string());
        }
        self.cockpit_snapshot(owner_session_id)
    }

    pub async fn start_native(
        &self,
        owner_session_id: Uuid,
        selection_token: &str,
        reviewed_target_app_id: &str,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _platform_guard = self.operation.lock().await;
        let _operation_guard = self.simulator_operation.lock().await;
        let index = self.simulator()?;
        if has_active_desktop_run(&index, owner_session_id)? {
            return Err("This session already has an active Computer Run".into());
        }
        self.prune_native_services(&index)?;
        let target = self
            .selections
            .lock()
            .map_err(|_| "Computer Use selection state is unavailable".to_string())?
            .remove(selection_token)
            .ok_or_else(|| {
                "Computer Use selection is stale; refresh the window list".to_string()
            })?;
        if reviewed_target_app_id != target.app_id {
            return Err("The reviewed Computer Use target no longer matches".into());
        }
        let platform = self.platform()?;
        let backend = platform
            .bind_target(selection_token)
            .await
            .map_err(|error| error.to_string())?;
        let store = self
            .store
            .clone()
            .ok_or_else(|| self.initialization_error())?;
        let service = Arc::new(ComputerUseService::new(backend, store));
        let limits = ComputerUseLimits {
            max_actions: 8,
            max_duration_secs: 10 * 60,
            max_observation_age_millis: 10_000,
            max_evidence_bytes: 16 * 1024 * 1024,
            ..ComputerUseLimits::default()
        };
        let run = service
            .create_run(
                &Uuid::new_v4().to_string(),
                owner_session_id,
                self.session_workspace(owner_session_id),
                target,
                limits,
            )
            .map_err(|error| error.to_string())?;
        self.native_services
            .lock()
            .map_err(|_| "Computer Use native run state is unavailable".to_string())?
            .insert(run.run_id.clone(), service.clone());
        self.clear_pending_for_owner(owner_session_id)?;
        if let Err(error) = authorize_and_observe_once(&service, &run).await {
            let _ = service
                .cancel(&Uuid::new_v4().to_string(), &run.run_id)
                .await;
            self.native_services
                .lock()
                .map_err(|_| "Computer Use native run state is unavailable".to_string())?
                .remove(&run.run_id);
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
        let (service, run) = self.owned_service(owner_session_id, run_id)?;
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
        authorize_and_observe_once(&service, &run)
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
        self.stage_action_locked(
            owner_session_id,
            run_id,
            expected_version,
            observation_id,
            action,
        )
    }

    fn stage_action_locked(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        action: ComputerAction,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let (_service, run) = self.owned_service(owner_session_id, run_id)?;
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

        let (action_summary, risk) = approval_copy(observation, &action)?;

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

    pub fn model_proposal_context(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
    ) -> Result<ComputerObservation, String> {
        if self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?
            .is_some()
        {
            return Err("Resolve or discard the current Computer Use approval first".into());
        }
        let (_service, run) = self.owned_service(owner_session_id, run_id)?;
        if run.version != expected_version || run.state != ComputerRunState::Ready {
            return Err("The Computer Run changed before the model request started".into());
        }
        run.current_observation
            .filter(|observation| observation.observation_id == observation_id)
            .ok_or_else(|| {
                "The Computer observation changed before the model request started".into()
            })
    }

    pub async fn apply_model_proposal(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
        expected_version: u64,
        observation_id: &str,
        proposal: ComputerAgentProposal,
    ) -> Result<ComputerAgentProposalResult, String> {
        let _guard = self.simulator_operation.lock().await;
        if proposal.observation_id() != observation_id {
            return Err("The model proposal does not match the requested observation".into());
        }
        match proposal {
            ComputerAgentProposal::Action {
                action, summary, ..
            } => {
                let snapshot = self.stage_action_locked(
                    owner_session_id,
                    run_id,
                    expected_version,
                    observation_id,
                    action,
                )?;
                Ok(ComputerAgentProposalResult {
                    snapshot,
                    summary,
                    completed: false,
                })
            }
            ComputerAgentProposal::Complete { summary, .. } => {
                let (service, run) = self.owned_service(owner_session_id, run_id)?;
                if run.version != expected_version
                    || run.state != ComputerRunState::Ready
                    || run
                        .current_observation
                        .as_ref()
                        .map(|observation| observation.observation_id.as_str())
                        != Some(observation_id)
                {
                    return Err("The Computer Run changed while the model was responding".into());
                }
                service
                    .complete(&Uuid::new_v4().to_string(), run_id, expected_version)
                    .map_err(|error| error.to_string())?;
                Ok(ComputerAgentProposalResult {
                    snapshot: self.cockpit_snapshot(owner_session_id)?,
                    summary,
                    completed: true,
                })
            }
        }
    }

    pub async fn approve_simulator_action(
        &self,
        owner_session_id: Uuid,
        approval_id: &str,
        request_id: &str,
    ) -> Result<ComputerCockpitSnapshot, String> {
        let _guard = self.simulator_operation.lock().await;
        let pending = self
            .pending_approval
            .lock()
            .map_err(|_| "Computer Use approval state is unavailable".to_string())?
            .clone()
            .filter(|pending| {
                pending.owner_session_id == owner_session_id && pending.approval_id == approval_id
            })
            .ok_or_else(|| "This Computer Use approval is stale or already resolved".to_string())?;
        let (service, run) = self.owned_service(owner_session_id, &pending.run_id)?;
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
        let (service, _) = self.owned_service(owner_session_id, run_id)?;
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
        let (service, _) = self.owned_service(owner_session_id, run_id)?;
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
        let (service, _) = self.owned_service(owner_session_id, run_id)?;
        service
            .cancel(&Uuid::new_v4().to_string(), run_id)
            .await
            .map_err(|error| error.to_string())?;
        self.cockpit_snapshot(owner_session_id)
    }

    fn simulator(&self) -> Result<Arc<ComputerUseService>, String> {
        self.simulator
            .clone()
            .ok_or_else(|| self.initialization_error())
    }

    fn owned_service(
        &self,
        owner_session_id: Uuid,
        run_id: &str,
    ) -> Result<(Arc<ComputerUseService>, ComputerRun), String> {
        let index = self.simulator()?;
        let run = owned_run(&index, owner_session_id, run_id)?;
        if run.target.app_id == SimulatorBackend::demo_target().app_id {
            return Ok((index, run));
        }
        let service = self
            .native_services
            .lock()
            .map_err(|_| "Computer Use native run state is unavailable".to_string())?
            .get(run_id)
            .cloned()
            .ok_or_else(|| {
                "Native Computer Run backend is unavailable after restart; start a new run"
                    .to_string()
            })?;
        Ok((service, run))
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

    fn prune_native_services(&self, index: &ComputerUseService) -> Result<(), String> {
        let active_run_ids = index
            .list_runs()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|run| !run.state.is_terminal())
            .map(|run| run.run_id)
            .collect::<BTreeSet<_>>();
        let mut services = self
            .native_services
            .lock()
            .map_err(|_| "Computer Use native run state is unavailable".to_string())?;
        services.retain(|run_id, _| active_run_ids.contains(run_id));
        if services.len() >= MAX_LIVE_NATIVE_SERVICES {
            return Err(format!(
                "At most {MAX_LIVE_NATIVE_SERVICES} native Computer Runs may remain active"
            ));
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

fn latest_desktop_run(
    service: &ComputerUseService,
    owner_session_id: Uuid,
) -> Result<Option<ComputerRun>, String> {
    service
        .list_runs()
        .map_err(|error| error.to_string())
        .map(|runs| {
            runs.into_iter()
                .filter(|run| run.owner_session_id == owner_session_id)
                .max_by_key(|run| run.updated_at)
        })
}

fn has_active_desktop_run(
    service: &ComputerUseService,
    owner_session_id: Uuid,
) -> Result<bool, String> {
    service
        .list_runs()
        .map_err(|error| error.to_string())
        .map(|runs| {
            runs.into_iter()
                .any(|run| run.owner_session_id == owner_session_id && !run.state.is_terminal())
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
        .filter(|run| run.owner_session_id == owner_session_id)
        .ok_or_else(|| "Computer Run does not belong to this session".to_string())
}

fn unavailable_native_capabilities() -> ComputerCapabilities {
    ComputerCapabilities {
        backend_id: "macos_interrupted".into(),
        observe: false,
        semantic_actions: false,
        text_entry: false,
        key_chords: false,
        pointer_fallback: false,
    }
}

fn approval_copy(
    observation: &ComputerObservation,
    action: &ComputerAction,
) -> Result<(String, String), String> {
    if matches!(action, ComputerAction::ActivateTarget) {
        return Ok((
            "Bring the authorized application to the foreground".into(),
            "Application focus".into(),
        ));
    }
    let element_id = action
        .referenced_element()
        .ok_or_else(|| "The proposed action does not identify an element".to_string())?;
    let element = observation
        .element(element_id)
        .filter(|element| element.enabled)
        .ok_or_else(|| "The proposed element is missing or disabled".to_string())?;
    let (required, verb, risk) = match action {
        ComputerAction::SetValue { .. } => (
            SemanticAction::SetValue,
            "Enter visible text in",
            "Text entry",
        ),
        ComputerAction::Invoke { .. } => (SemanticAction::Invoke, "Invoke", "Semantic action"),
        ComputerAction::Select { .. } => (SemanticAction::Select, "Select", "Semantic action"),
        ComputerAction::Scroll { .. } => (SemanticAction::Scroll, "Scroll to", "Semantic action"),
        _ => return Err("The cockpit accepts semantic Accessibility actions only".into()),
    };
    if element.sensitivity.is_hard_denied() || !element.actions.contains(&required) {
        return Err("The proposed semantic action is outside the observed scope".into());
    }
    let label = element.label.as_deref().unwrap_or(&element.role);
    Ok((format!("{verb} {label}"), risk.into()))
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
        executor: None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use grokptah_agent_bridge::computer_use::{ObservationGeometry, SemanticElement, Sensitivity};
    use grokptah_agent_bridge::{ActionOutcome, ComputerBackend, ComputerStore, ComputerTarget};

    use super::*;

    const NATIVE_TEST_APP_ID: &str = "com.example.grokptah-native-fixture";

    #[derive(Debug)]
    struct NativeTestBackend {
        target: ComputerTarget,
        actions: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ComputerBackend for NativeTestBackend {
        fn capabilities(&self) -> ComputerCapabilities {
            ComputerCapabilities {
                backend_id: "native_test_backend".into(),
                observe: true,
                semantic_actions: true,
                text_entry: true,
                key_chords: false,
                pointer_fallback: false,
            }
        }

        async fn observe(
            &self,
            _run_id: &str,
            observation_id: &str,
            target: &ComputerTarget,
            limits: &ComputerUseLimits,
        ) -> Result<ComputerObservation, ComputerError> {
            if target != &self.target {
                return Err(ComputerError::new(
                    grokptah_agent_bridge::ComputerErrorCode::ForbiddenTarget,
                    "test target changed",
                ));
            }
            let observation = ComputerObservation {
                observation_id: observation_id.to_string(),
                sequence: 1,
                target: self.target.clone(),
                captured_at: Utc::now(),
                geometry: ObservationGeometry {
                    x: 0.0,
                    y: 0.0,
                    width: 720.0,
                    height: 520.0,
                    scale_factor: 1.0,
                },
                screenshot: None,
                elements: vec![SemanticElement {
                    element_id: "native-name".into(),
                    role: "AXTextField".into(),
                    label: Some("Project label".into()),
                    value: Some("before".into()),
                    bounds: None,
                    enabled: true,
                    focused: false,
                    sensitivity: Sensitivity::None,
                    actions: BTreeSet::from([SemanticAction::SetValue]),
                }],
                elements_truncated: false,
                sensitivity: Sensitivity::None,
            };
            observation.validate(limits)?;
            Ok(observation)
        }

        async fn act(
            &self,
            _run_id: &str,
            observation: &ComputerObservation,
            action: &ComputerAction,
        ) -> Result<ActionOutcome, ComputerError> {
            if observation.target != self.target
                || !matches!(
                    action,
                    ComputerAction::SetValue { element_id, .. } if element_id == "native-name"
                )
            {
                return Err(ComputerError::new(
                    grokptah_agent_bridge::ComputerErrorCode::ForbiddenAction,
                    "test action escaped its semantic binding",
                ));
            }
            self.actions.fetch_add(1, Ordering::SeqCst);
            Ok(ActionOutcome::bounded("native test action", Some(true)))
        }

        async fn cancel(&self, _run_id: &str) -> Result<(), ComputerError> {
            Ok(())
        }
    }

    #[derive(Debug)]
    struct NativeTestPlatform {
        candidate: ComputerTargetCandidate,
        actions: Arc<AtomicUsize>,
        available: std::sync::Mutex<bool>,
    }

    #[async_trait]
    impl ComputerObservationPlatform for NativeTestPlatform {
        fn status(&self) -> ComputerPlatformStatus {
            ComputerPlatformStatus {
                platform_id: "test_macos".into(),
                available: true,
                minimum_os_version: None,
                screen_recording: ComputerPermissionStatus::Granted,
                accessibility: ComputerPermissionStatus::Granted,
                detail: None,
                executor: None,
            }
        }

        async fn request_permission(
            &self,
            _permission: ComputerPermission,
        ) -> Result<ComputerPermissionStatus, ComputerError> {
            Ok(ComputerPermissionStatus::Granted)
        }

        async fn list_targets(&self) -> Result<Vec<ComputerTargetCandidate>, ComputerError> {
            *self.available.lock().unwrap() = true;
            Ok(vec![self.candidate.clone()])
        }

        async fn bind_target(
            &self,
            selection_token: &str,
        ) -> Result<Arc<dyn ComputerBackend>, ComputerError> {
            let mut available = self.available.lock().unwrap();
            if selection_token != self.candidate.selection_token || !*available {
                return Err(ComputerError::new(
                    grokptah_agent_bridge::ComputerErrorCode::Unauthorized,
                    "test selection is stale",
                ));
            }
            *available = false;
            Ok(Arc::new(NativeTestBackend {
                target: self.candidate.target.clone(),
                actions: self.actions.clone(),
            }))
        }
    }

    /// Host fixture with its persist directories bound under the disposable
    /// fixture directory. The process-global home override is serialized and
    /// restored so parallel tests never touch the real user home.
    fn test_host(dir: &std::path::Path) -> AgentHostHandle {
        let _guard = grokptah_agent_bridge::home_override_serial();
        grokptah_agent_bridge::set_grokptah_home_override(Some(dir.join(".grokptah")));
        let host = grokptah_agent_bridge::AgentHost::create(Default::default());
        grokptah_agent_bridge::set_grokptah_home_override(None);
        host
    }

    fn test_desktop() -> (tempfile::TempDir, DesktopComputerUse) {
        let dir = tempfile::tempdir().unwrap();
        // Tests deliberately open an isolated store in the fixture directory;
        // production `new()` must borrow the host's shared handle instead.
        let store = ComputerStore::open(dir.path().join("computer-use")).unwrap();
        let simulator = Arc::new(ComputerUseService::new(
            Arc::new(SimulatorBackend::new()),
            store.clone(),
        ));
        // Build the host before `dir` moves into the returned tuple.
        let host = test_host(dir.path());
        (
            dir,
            DesktopComputerUse {
                host,
                platform: None,
                store: Some(store),
                initialization_error: None,
                operation: Mutex::new(()),
                selections: std::sync::Mutex::new(HashMap::new()),
                simulator: Some(simulator),
                native_services: std::sync::Mutex::new(HashMap::new()),
                simulator_operation: Mutex::new(()),
                pending_approval: std::sync::Mutex::new(None),
            },
        )
    }

    fn native_test_desktop() -> (tempfile::TempDir, DesktopComputerUse, Arc<AtomicUsize>) {
        let (dir, mut desktop) = test_desktop();
        let actions = Arc::new(AtomicUsize::new(0));
        let target = ComputerTarget {
            app_id: NATIVE_TEST_APP_ID.into(),
            window_id: "native-window".into(),
            generation: 7,
            display_name: "Disposable Native Fixture".into(),
            sensitivity: Sensitivity::None,
        };
        desktop.platform = Some(Arc::new(NativeTestPlatform {
            candidate: ComputerTargetCandidate {
                selection_token: "native-selection".into(),
                target,
                geometry: ObservationGeometry {
                    x: 0.0,
                    y: 0.0,
                    width: 720.0,
                    height: 520.0,
                    scale_factor: 1.0,
                },
                on_screen: true,
                active: true,
                minimized: false,
            },
            actions: actions.clone(),
            available: std::sync::Mutex::new(true),
        }));
        (dir, desktop, actions)
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
    async fn model_proposal_is_revalidated_and_staged_without_dispatch() {
        let (_dir, desktop) = test_desktop();
        let owner = Uuid::new_v4();
        let target = SimulatorBackend::demo_target();
        let started = desktop
            .start_simulator(owner, &target.app_id)
            .await
            .unwrap();
        let run = started.run.unwrap();
        let observation = run.current_observation.as_ref().unwrap();
        let result = desktop
            .apply_model_proposal(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAgentProposal::Action {
                    observation_id: observation.observation_id.clone(),
                    action: ComputerAction::SetValue {
                        element_id: format!("{}-name", observation.observation_id),
                        text: "Ada Lovelace".into(),
                    },
                    summary: "Enter the visible name".into(),
                },
            )
            .await
            .unwrap();
        assert!(!result.completed);
        assert!(result.snapshot.pending_approval.is_some());
        assert_eq!(result.snapshot.run.unwrap().action_count, 0);

        let stale = desktop
            .apply_model_proposal(
                owner,
                &run.run_id,
                run.version,
                "stale-observation",
                ComputerAgentProposal::Action {
                    observation_id: "stale-observation".into(),
                    action: ComputerAction::ActivateTarget,
                    summary: "stale".into(),
                },
            )
            .await;
        assert!(stale.is_err());
    }

    #[tokio::test]
    async fn model_completion_only_revokes_authority_on_exact_current_frame() {
        let (_dir, desktop) = test_desktop();
        let owner = Uuid::new_v4();
        let target = SimulatorBackend::demo_target();
        let started = desktop
            .start_simulator(owner, &target.app_id)
            .await
            .unwrap();
        let run = started.run.unwrap();
        let observation = run.current_observation.as_ref().unwrap();
        let result = desktop
            .apply_model_proposal(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAgentProposal::Complete {
                    observation_id: observation.observation_id.clone(),
                    summary: "The visible objective is complete".into(),
                },
            )
            .await
            .unwrap();
        assert!(result.completed);
        let completed = result.snapshot.run.unwrap();
        assert_eq!(completed.state, ComputerRunState::Completed);
        assert!(completed
            .grant
            .as_ref()
            .is_some_and(|grant| grant.revoked_at.is_some()));
        assert_eq!(completed.action_count, 0);
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

    #[tokio::test]
    async fn native_run_uses_the_same_exact_one_use_approval_path() {
        let (_dir, desktop, actions) = native_test_desktop();
        let owner = Uuid::new_v4();
        let candidate = desktop.list_targets().await.unwrap().remove(0);
        let started = desktop
            .start_native(owner, &candidate.selection_token, NATIVE_TEST_APP_ID)
            .await
            .unwrap();
        assert_eq!(started.backend.backend_id, "native_test_backend");
        let run = started.run.unwrap();
        let observation = run.current_observation.as_ref().unwrap();
        let staged = desktop
            .stage_simulator_action(
                owner,
                &run.run_id,
                run.version,
                &observation.observation_id,
                ComputerAction::SetValue {
                    element_id: "native-name".into(),
                    text: "after".into(),
                },
            )
            .await
            .unwrap();
        let approval = staged.pending_approval.unwrap();
        assert_eq!(
            approval.action_summary,
            "Enter visible text in Project label"
        );
        let acted = desktop
            .approve_simulator_action(owner, &approval.approval_id, "native-approve-once")
            .await
            .unwrap();
        assert_eq!(actions.load(Ordering::SeqCst), 1);
        assert_eq!(acted.run.unwrap().state, ComputerRunState::Paused);
        assert!(desktop
            .approve_simulator_action(owner, &approval.approval_id, "native-approve-once")
            .await
            .is_err());
        assert_eq!(actions.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn starting_a_native_run_prunes_terminal_native_backends() {
        let (_dir, desktop, _actions) = native_test_desktop();
        let first_owner = Uuid::new_v4();
        let candidate = desktop.list_targets().await.unwrap().remove(0);
        let first = desktop
            .start_native(first_owner, &candidate.selection_token, NATIVE_TEST_APP_ID)
            .await
            .unwrap()
            .run
            .unwrap();
        desktop
            .stop_simulator(first_owner, &first.run_id)
            .await
            .unwrap();
        assert_eq!(desktop.native_services.lock().unwrap().len(), 1);

        let second_owner = Uuid::new_v4();
        let candidate = desktop.list_targets().await.unwrap().remove(0);
        let second = desktop
            .start_native(second_owner, &candidate.selection_token, NATIVE_TEST_APP_ID)
            .await
            .unwrap()
            .run
            .unwrap();
        let services = desktop.native_services.lock().unwrap();
        assert_eq!(services.len(), 1);
        assert!(!services.contains_key(&first.run_id));
        assert!(services.contains_key(&second.run_id));
    }
}
