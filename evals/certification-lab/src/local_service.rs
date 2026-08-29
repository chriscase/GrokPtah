//! Isolated, in-process GrokPtah service bootstrap for certification campaigns.
//!
//! This module is intentionally a narrow lifecycle adapter. It constructs the
//! shipped orchestration and MCP control plane from public bridge APIs, then
//! exposes only network-facing client information. Certification scenarios
//! must use [`McpControlClient`] and must never inspect the host or store.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::provider_observation::ProviderObservationSession;
use grokptah_agent_bridge::{
    home_override_serial, start_control_server_with, AgentHost, ControlServerHandle,
    ControlServerLimits, HostConfig, HostRuntime, McpControlClient, OrchestrationConfig,
    OrchestrationService, RunBounds, RuntimeHome, WorkspaceAllowlist,
};
use uuid::Uuid;

const OFFLINE_ENV: &str = "GROKPTAH_AGENT_OFFLINE";
pub const DEFAULT_LIVE_MODEL: &str = "grok-build";

/// Whether locally hosted model turns use the deterministic offline runtime or
/// the configured GrokPtah provider route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalServiceMode {
    Offline,
    Live,
}

/// Exact roots and bounds used to start an isolated loopback control plane.
///
/// Callers own creation and later cleanup of these disposable directories.
/// The configuration never falls back to ambient home or workspace discovery.
#[derive(Clone)]
pub struct LocalServiceConfig {
    runtime_home: PathBuf,
    workspaces: Vec<PathBuf>,
    mode: LocalServiceMode,
    max_concurrent: usize,
    max_rounds: u32,
    max_duration_ms: u64,
    max_run_tokens: u64,
    model: String,
    provider_observation: Option<ProviderObservationSession>,
}

impl LocalServiceConfig {
    pub fn new(
        runtime_home: impl Into<PathBuf>,
        workspaces: Vec<PathBuf>,
        mode: LocalServiceMode,
        max_concurrent: usize,
        max_rounds: u32,
        max_duration_ms: u64,
        max_run_tokens: u64,
    ) -> Self {
        Self {
            runtime_home: runtime_home.into(),
            workspaces,
            mode,
            max_concurrent,
            max_rounds,
            max_duration_ms,
            max_run_tokens,
            model: DEFAULT_LIVE_MODEL.to_owned(),
            provider_observation: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Result<Self> {
        let model = model.into();
        validate_public_model(&model)?;
        self.model = model;
        Ok(self)
    }

    pub fn with_provider_observation(mut self, session: ProviderObservationSession) -> Self {
        self.provider_observation = Some(session);
        self
    }

    fn validate_and_canonicalize(mut self) -> Result<Self> {
        if self.workspaces.is_empty() {
            bail!("at least one disposable workspace root is required");
        }
        if self.max_concurrent == 0 {
            bail!("max_concurrent must be greater than zero");
        }
        if self.max_rounds == 0 {
            bail!("max_rounds must be greater than zero");
        }
        if self.max_duration_ms == 0 || self.max_run_tokens == 0 {
            bail!("local Run duration and token ceilings must be greater than zero");
        }
        validate_public_model(&self.model)?;

        let mut unique = HashSet::with_capacity(self.workspaces.len());
        let mut workspaces = Vec::with_capacity(self.workspaces.len());
        for workspace in self.workspaces {
            if !workspace.is_dir() {
                bail!("every disposable workspace root must be an existing directory");
            }
            let workspace =
                dunce::canonicalize(workspace).context("canonicalize disposable workspace root")?;
            if !unique.insert(workspace.clone()) {
                bail!("disposable workspace roots must be unique");
            }
            workspaces.push(workspace);
        }

        let runtime_home = RuntimeHome::from_path(&self.runtime_home)
            .context("prepare disposable GrokPtah runtime home")?;
        let runtime_home = runtime_home.path().to_path_buf();
        for workspace in &workspaces {
            if paths_overlap(&runtime_home, workspace) {
                bail!("runtime home and scenario workspace roots must not overlap");
            }
        }

        self.runtime_home = runtime_home;
        self.workspaces = workspaces;
        Ok(self)
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

/// Running loopback control plane for an owned certification campaign.
///
/// Host and store handles remain private by design. Holding this value also
/// holds a scoped process-environment guard because the bridge's deterministic
/// offline switch and compatibility runtime-home context are process-wide.
pub struct LocalService {
    config: LocalServiceConfig,
    token: String,
    base_url: String,
    server: Option<ControlServerHandle>,
    /// The single non-cloneable owner of the lab's disposable home: it owns the
    /// instance lock and the task supervisor, and its ordered shutdown is what
    /// releases them (#455).
    host: Option<HostRuntime>,
    _process_environment: ProcessEnvironment,
}

impl LocalService {
    /// Start an isolated loopback service with a fresh in-memory bearer token.
    #[allow(clippy::await_holding_lock)]
    pub async fn start(config: LocalServiceConfig) -> Result<Self> {
        let config = config.validate_and_canonicalize()?;
        // RuntimeHome installs a process-wide compatibility context. Reuse the
        // bridge's public serialization guard and hold it for the whole host
        // lifetime; the same guard also makes the offline env override sound.
        let process_environment = ProcessEnvironment::enter(config.mode);
        let token = generated_token();
        let (host, server, base_url) = bootstrap(&config, &token).await?;
        Ok(Self {
            config,
            token,
            base_url,
            server: Some(server),
            host: Some(host),
            _process_environment: process_environment,
        })
    }

    /// Base URL accepted by [`McpControlClient`] (the client appends `/mcp`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Explicit public model profile selected for this owned service.
    pub fn model_identity(&self) -> &str {
        &self.config.model
    }

    /// Construct an uninitialized black-box client for the public MCP surface.
    pub fn client(&self) -> McpControlClient {
        McpControlClient::new(self.base_url.clone(), self.token.clone())
    }

    /// Stop and reopen the control plane against the exact same durable home.
    ///
    /// The listener uses another ephemeral loopback port, while the in-memory
    /// bearer and disposable workspace allowlist remain unchanged.
    #[allow(clippy::await_holding_lock)]
    pub async fn restart(&mut self) -> Result<()> {
        self.shutdown_parts().await;
        let (host, server, base_url) = bootstrap(&self.config, &self.token)
            .await
            .context("restart isolated GrokPtah control plane")?;
        self.host = Some(host);
        self.server = Some(server);
        self.base_url = base_url;
        Ok(())
    }

    /// Gracefully release the control server, background supervisors, and host.
    #[allow(clippy::await_holding_lock)]
    pub async fn stop(mut self) {
        self.shutdown_parts().await;
    }

    async fn shutdown_parts(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop_and_wait().await;
        }
        if let Some(host) = self.host.take() {
            // Ordered shutdown: join every supervised task, flush durable
            // state, then release the instance lock exactly once, so the next
            // lab run can reopen the same disposable home immediately (#455).
            let report = host.shutdown().await;
            if !report.is_clean() {
                eprintln!(
                    "[certification-lab] host shutdown: {}",
                    report.operator_summary()
                );
            }
        }
    }
}

impl Drop for LocalService {
    fn drop(&mut self) {
        // Explicit `stop` is preferred because it waits for supervisors. This
        // fail-safe still closes admission immediately during unwinding.
        if let Some(server) = self.server.take() {
            server.stop();
        }
        if let Some(host) = self.host.take() {
            let _ = host.stop();
        }
    }
}

async fn bootstrap(
    config: &LocalServiceConfig,
    token: &str,
) -> Result<(HostRuntime, ControlServerHandle, String)> {
    let runtime_home = RuntimeHome::from_path(&config.runtime_home)
        .context("reopen disposable GrokPtah runtime home")?;
    let host = AgentHost::create_with_runtime_home(
        HostConfig {
            default_model: config.model.clone(),
            always_approve: false,
            max_agent_rounds: Some(config.max_rounds),
            provider_observation: config.provider_observation.clone(),
            ..HostConfig::default()
        },
        runtime_home,
    )
    .context("acquire the GrokPtah single-instance lock for the disposable lab home")?;
    host.start().context("start isolated GrokPtah host")?;

    let store = match host.ensure_orchestration_store() {
        Ok(store) => store,
        Err(error) => {
            let _ = host.stop();
            return Err(error).context("open isolated durable orchestration store");
        }
    };
    let allowlist = WorkspaceAllowlist::new(config.workspaces.clone());
    if allowlist.roots().len() != config.workspaces.len() {
        let _ = host.stop();
        bail!("every workspace root must remain canonical and accessible at startup");
    }

    let bounds = RunBounds {
        max_rounds: config.max_rounds,
        max_duration_ms: config.max_duration_ms,
        max_total_tokens: Some(config.max_run_tokens),
        ..RunBounds::default()
    };
    let orchestration = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: token.to_owned(),
            allowlist,
            max_concurrent_runs: config.max_concurrent,
            bounds,
        },
    );
    let limits = ControlServerLimits {
        max_concurrent: config.max_concurrent,
        ..ControlServerLimits::default()
    };
    let server = match start_control_server_with(orchestration, 0, limits).await {
        Ok(mut server) => {
            server.token = token.to_owned();
            server
        }
        Err(error) => {
            let _ = host.stop();
            return Err(error).context("bind isolated loopback MCP control plane");
        }
    };
    let base_url = format!("http://{}", server.addr);
    Ok((host, server, base_url))
}

fn generated_token() -> String {
    format!(
        "cert-{}{}",
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    )
}

pub(crate) fn validate_public_model(model: &str) -> Result<()> {
    if !(6..=80).contains(&model.len())
        || !model.starts_with("grok-")
        || !model.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'.' | b'_')
        })
    {
        bail!("live_model_invalid");
    }
    Ok(())
}

struct ProcessEnvironment {
    previous_offline: Option<OsString>,
    _serial: std::sync::MutexGuard<'static, ()>,
}

impl ProcessEnvironment {
    fn enter(mode: LocalServiceMode) -> Self {
        let serial = home_override_serial();
        let previous_offline = std::env::var_os(OFFLINE_ENV);
        // SAFETY: `home_override_serial` is held until this override is
        // restored. Local certification hosts therefore cannot race each
        // other or the bridge/service harnesses that use the same guard.
        unsafe {
            match mode {
                LocalServiceMode::Offline => std::env::set_var(OFFLINE_ENV, "1"),
                LocalServiceMode::Live => std::env::remove_var(OFFLINE_ENV),
            }
        }
        Self {
            previous_offline,
            _serial: serial,
        }
    }
}

impl Drop for ProcessEnvironment {
    fn drop(&mut self) {
        // SAFETY: the serialization guard is still held and this restores the
        // exact value observed before `enter`.
        unsafe {
            match self.previous_offline.take() {
                Some(value) => std::env::set_var(OFFLINE_ENV, value),
                None => std::env::remove_var(OFFLINE_ENV),
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn loopback_test_available() -> bool {
    match std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            eprintln!("loopback integration skipped: execution sandbox forbids listeners");
            false
        }
        Err(error) => panic!("unexpected loopback availability failure: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn offline_control_plane_is_black_box_and_restart_durable() {
        if !loopback_test_available() {
            return;
        }
        let previous_offline = std::env::var_os(OFFLINE_ENV);
        let root = tempdir().expect("disposable lab root");
        let home = root.path().join("home");
        let workspace_a = root.path().join("workspace-a");
        let workspace_b = root.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("first workspace");
        std::fs::create_dir_all(&workspace_b).expect("second workspace");

        let config = LocalServiceConfig::new(
            &home,
            vec![workspace_a.clone(), workspace_b],
            LocalServiceMode::Offline,
            2,
            3,
            60_000,
            20_000,
        );
        let mut service = LocalService::start(config).await.expect("start service");
        assert_eq!(std::env::var_os(OFFLINE_ENV), Some(OsString::from("1")));
        assert!(service.base_url().starts_with("http://127.0.0.1:"));
        assert_eq!(service.model_identity(), DEFAULT_LIVE_MODEL);

        let mut client = service.client();
        client.initialize().await.expect("initialize client");
        let created = client
            .call_tool(
                "ptah_create_session",
                json!({
                    "workspace": dunce::canonicalize(&workspace_a).expect("workspace"),
                    "title": "Restart durability probe",
                }),
            )
            .await
            .expect("create session through MCP");
        assert!(!created.is_error, "create session failed");
        let session_id = created.structured["sessionId"]
            .as_str()
            .expect("session id")
            .to_owned();
        client.close_session().await.expect("close first client");

        service.restart().await.expect("restart service");
        let mut reconnected = service.client();
        reconnected.initialize().await.expect("reconnect client");
        let sessions = reconnected
            .call_tool("ptah_list_sessions", json!({}))
            .await
            .expect("list sessions through MCP");
        assert!(!sessions.is_error, "list sessions failed");
        assert!(sessions.structured["sessions"]
            .as_array()
            .expect("sessions array")
            .iter()
            .any(|session| session["sessionId"] == session_id));
        reconnected.close_session().await.expect("close client");

        service.stop().await;
        assert_eq!(std::env::var_os(OFFLINE_ENV), previous_offline);
    }

    #[test]
    fn local_model_profile_is_explicit_and_public() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let config = LocalServiceConfig::new(
            root.path().join("home"),
            vec![workspace],
            LocalServiceMode::Live,
            1,
            1,
            60_000,
            20_000,
        );
        assert!(config.clone().with_model("opaque-private-route").is_err());
        assert!(config.with_model("grok-build").is_ok());
    }
}
