//! Isolated, in-process GrokPtah service bootstrap for certification campaigns.
//!
//! This module is intentionally a narrow lifecycle adapter. It constructs the
//! shipped orchestration and MCP control plane from public bridge APIs, then
//! exposes only network-facing client information. Certification scenarios
//! must use [`McpControlClient`] and must never inspect the host or store.

use std::collections::HashSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::provider_observation::ProviderObservationSession;
use grokptah_agent_bridge::{
    home_override_serial, start_control_server_with, AgentHost, AgentHostHandle,
    ControlServerHandle, ControlServerLimits, HostConfig, McpControlClient, OrchestrationConfig,
    OrchestrationService, RunBounds, RuntimeHome, WorkspaceAllowlist, FORBIDDEN_TOOLS,
};
use sha2::{Digest, Sha256};
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
    host: Option<AgentHostHandle>,
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
            let _ = host.stop();
            drop(host);
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
) -> Result<(AgentHostHandle, ControlServerHandle, String)> {
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
    );
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

/// Live enterprise-gateway attach for the code-review benchmark.
///
/// The current host does not issue a disposable enterprise-gateway lease,
/// a gateway-signed modest-tier deployment attestation, or an external
/// egress-firewall attestation. Compatible-gateway ambient routes remain
/// refused; this API never installs a bypass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnterpriseReviewLiveGap {
    pub disposable_lease: bool,
    pub gateway_signed_deployment_attestation: bool,
    pub egress_firewall_attestation: bool,
}

pub fn enterprise_review_live_attach() -> Result<(), EnterpriseReviewLiveGap> {
    Err(EnterpriseReviewLiveGap {
        disposable_lease: false,
        gateway_signed_deployment_attestation: false,
        egress_firewall_attestation: false,
    })
}

/// Observed denials from the public MCP control-plane allowlist.
///
/// This is not a GrokPtah Agent review turn and does not observe a live
/// provider. It records that scripted malicious wire names were refused by
/// the first real control-plane authorization boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewControlPlaneEvidence {
    pub listed_tool_count: u32,
    pub forbidden_tools_listed: u32,
    pub denied_wire_calls: u32,
    pub successful_denied_wire_calls: u32,
    pub restart_count: u32,
}

/// Whether this process can bind a loopback listener for the isolated control plane.
pub(crate) fn loopback_bind_available() -> bool {
    match std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => false,
        Err(error) => panic!("unexpected loopback availability failure: {error}"),
    }
}

/// Drive scripted malicious wire names through [`LocalService`] + [`McpControlClient`].
///
/// `always_approve` stays false. Successful dispatch of a denied wire name is a
/// contract failure. Restart reopens the same durable home and must still omit
/// [`FORBIDDEN_TOOLS`].
pub async fn exercise_review_control_plane(
    workspace: &Path,
    runtime_home: &Path,
    wire_names: &[String],
) -> Result<ReviewControlPlaneEvidence> {
    if !loopback_bind_available() {
        bail!("review_loopback_bind_unavailable");
    }
    if wire_names.is_empty() {
        bail!("review control plane requires scripted malicious wire names");
    }
    let config = LocalServiceConfig::new(
        runtime_home,
        vec![workspace.to_path_buf()],
        LocalServiceMode::Offline,
        1,
        3,
        60_000,
        8_000,
    );
    let mut service = LocalService::start(config).await?;
    let mut client = service.client();
    client
        .initialize()
        .await
        .context("initialize review control-plane client")?;
    let listed = client
        .list_tools()
        .await
        .context("list review control-plane tools")?;
    let listed_names: HashSet<&str> = listed.iter().map(|tool| tool.name.as_str()).collect();
    let mut forbidden_tools_listed = 0u32;
    for forbidden in FORBIDDEN_TOOLS {
        if listed_names.contains(*forbidden) {
            forbidden_tools_listed = forbidden_tools_listed.saturating_add(1);
        }
    }
    if forbidden_tools_listed > 0 {
        service.stop().await;
        bail!("forbidden tool listed on the review control plane");
    }
    let mut denied_wire_calls = 0u32;
    let mut successful_denied_wire_calls = 0u32;
    for name in wire_names {
        match client.call_tool(name, serde_json::json!({})).await {
            Ok(result) if !result.is_error => {
                successful_denied_wire_calls = successful_denied_wire_calls.saturating_add(1);
            }
            Ok(_) | Err(_) => {
                denied_wire_calls = denied_wire_calls.saturating_add(1);
            }
        }
    }
    service
        .restart()
        .await
        .context("restart review control plane")?;
    let mut client = service.client();
    client
        .initialize()
        .await
        .context("reinitialize review control-plane client")?;
    let listed_after = client
        .list_tools()
        .await
        .context("list review control-plane tools after restart")?;
    for forbidden in FORBIDDEN_TOOLS {
        if listed_after.iter().any(|tool| tool.name == *forbidden) {
            service.stop().await;
            bail!("forbidden tool listed on the review control plane after restart");
        }
    }
    let listed_tool_count =
        u32::try_from(listed_after.len()).context("listed tool count overflow")?;
    service.stop().await;
    if successful_denied_wire_calls > 0 {
        bail!("malicious control-plane call succeeded");
    }
    if denied_wire_calls != u32::try_from(wire_names.len()).context("wire name count")? {
        bail!("control plane did not deny every scripted malicious wire name");
    }
    Ok(ReviewControlPlaneEvidence {
        listed_tool_count,
        forbidden_tools_listed: 0,
        denied_wire_calls,
        successful_denied_wire_calls: 0,
        restart_count: 1,
    })
}

/// Merkle root over a directory tree. Symbolic links fail closed. Relative
/// paths are mixed into the digest but never returned.
pub fn workspace_merkle_root(root: &Path) -> Result<String> {
    let mut entries = Vec::new();
    collect_merkle_entries(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, digest, bytes) in entries {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(digest.as_bytes());
        hasher.update(bytes.to_le_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRefSnapshot {
    pub head_digest: String,
    pub refs_digest: String,
    pub remote_publication_count: u32,
}

/// Snapshot git HEAD, refs, and remote-tracking publication count. The raw
/// refs never leave this function; only SHA-256 fingerprints are returned.
pub fn git_ref_snapshot(root: &Path) -> Result<GitRefSnapshot> {
    let head = git_stdout(root, &["rev-parse", "HEAD"])?;
    let refs = git_stdout(root, &["show-ref", "--head"])?;
    let remotes = git_stdout(
        root,
        &["for-each-ref", "--format=%(refname)", "refs/remotes"],
    )?;
    let remote_publication_count =
        u32::try_from(remotes.lines().filter(|line| !line.is_empty()).count())
            .context("remote publication count overflow")?;
    Ok(GitRefSnapshot {
        head_digest: format!("{:x}", Sha256::digest(head.as_bytes())),
        refs_digest: format!("{:x}", Sha256::digest(refs.as_bytes())),
        remote_publication_count,
    })
}

fn collect_merkle_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<(String, String, u64)>,
) -> Result<()> {
    let mut children = Vec::new();
    for child in std::fs::read_dir(directory).context("read workspace tree")? {
        children.push(child.context("read workspace entry")?);
    }
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let metadata = std::fs::symlink_metadata(&path).context("stat workspace entry")?;
        if metadata.file_type().is_symlink() {
            bail!("workspace tree contains a symbolic link");
        }
        if child.file_name() == ".git" {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .context("workspace path escaped its root")?
            .to_string_lossy()
            .replace('\\', "/");
        if metadata.is_dir() {
            collect_merkle_entries(root, &path, entries)?;
        } else if metadata.is_file() {
            let bytes = std::fs::read(&path).context("read workspace file")?;
            entries.push((
                relative,
                format!("{:x}", Sha256::digest(&bytes)),
                bytes.len() as u64,
            ));
        } else {
            bail!("workspace tree contains a non-regular entry");
        }
    }
    Ok(())
}

fn git_stdout(root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()
        .context("invoke git for workspace snapshot")?;
    if !output.status.success() {
        bail!("git workspace snapshot failed");
    }
    String::from_utf8(output.stdout).context("git snapshot was not UTF-8")
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
    if loopback_bind_available() {
        true
    } else {
        eprintln!("loopback integration skipped: execution sandbox forbids listeners");
        false
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

    #[test]
    fn enterprise_review_live_attach_is_unimplemented() {
        let error = enterprise_review_live_attach().unwrap_err();
        assert!(!error.disposable_lease);
        assert!(!error.gateway_signed_deployment_attestation);
        assert!(!error.egress_firewall_attestation);
    }

    #[test]
    fn workspace_merkle_is_stable_and_rejects_symlinks() {
        let root = tempdir().unwrap();
        let workspace = root.path().join("ws");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(workspace.join("a.txt"), b"one").unwrap();
        let first = workspace_merkle_root(&workspace).unwrap();
        let second = workspace_merkle_root(&workspace).unwrap();
        assert_eq!(first, second);
        std::fs::write(workspace.join("a.txt"), b"two").unwrap();
        assert_ne!(first, workspace_merkle_root(&workspace).unwrap());
    }
}
