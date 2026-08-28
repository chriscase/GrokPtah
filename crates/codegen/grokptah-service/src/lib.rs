//! Standalone headless GrokPtah service.
//!
//! The service owns process startup and configuration only. Agent execution,
//! durable orchestration, MCP authorization, and event recovery remain in the
//! shared `grokptah-agent-bridge` crate used by the desktop client.

use std::collections::HashSet;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    start_control_server_with_bind, AgentHost, AgentHostHandle, AuthCredential,
    ControlServerLimits, HostConfig, HostRuntime, HostShutdownReport, OrchStore,
    OrchestrationConfig, OrchestrationService, RuntimeHome, WorkspaceAllowlist,
};

pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_LISTEN: &str = "127.0.0.1:39200";
const DEFAULT_MAX_CONCURRENT: usize = 4;
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 120_000;

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub listen: SocketAddr,
    pub token: String,
    pub workspaces: Vec<PathBuf>,
    pub allow_remote: bool,
    pub max_concurrent: usize,
    pub request_timeout: Duration,
    /// Named device/client credentials. `primary` remains the compatibility
    /// credential represented by `token`.
    pub client_credentials: Vec<AuthCredential>,
    /// Account identity owning Agents on this service deployment. Multiple
    /// device credentials may share one owner while remaining attributable as
    /// distinct clients in audit and Run records.
    pub agent_owner_id: String,
    /// Explicit durable root for embedders and hosted deployments. `None`
    /// preserves the `GROKPTAH_HOME`/desktop discovery behavior.
    pub runtime_home: Option<RuntimeHome>,
}

impl ServiceConfig {
    pub fn new(
        listen: SocketAddr,
        token: impl Into<String>,
        workspaces: Vec<PathBuf>,
        allow_remote: bool,
        max_concurrent: usize,
        request_timeout: Duration,
    ) -> Result<Self> {
        let token = token.into().trim().to_string();
        let config = Self {
            listen,
            token: token.clone(),
            workspaces,
            allow_remote,
            max_concurrent,
            request_timeout,
            client_credentials: if token.is_empty() {
                Vec::new()
            } else {
                vec![AuthCredential::new("primary", token)
                    .map_err(|error| anyhow::anyhow!(error.message))?]
            },
            agent_owner_id: "primary".into(),
            runtime_home: None,
        };
        config.validate()?;
        Ok(config)
    }

    fn from_environment() -> Result<Self> {
        let listen = if let Ok(value) =
            env::var("GROKPTAH_SERVICE_LISTEN").or_else(|_| env::var("GROKPTAH_CONTROL_LISTEN"))
        {
            value
                .parse()
                .context("GROKPTAH_SERVICE_LISTEN must be an address such as 127.0.0.1:39200")?
        } else if let Ok(port) = env::var("GROKPTAH_CONTROL_PORT") {
            format!("127.0.0.1:{port}")
                .parse()
                .context("GROKPTAH_CONTROL_PORT must be a valid TCP port")?
        } else {
            DEFAULT_LISTEN
                .parse()
                .expect("the default service listen address is valid")
        };
        let token = env::var("GROKPTAH_SERVICE_TOKEN")
            .or_else(|_| env::var("GROKPTAH_CONTROL_TOKEN"))
            .unwrap_or_default();
        let mut client_credentials = if token.trim().is_empty() {
            Vec::new()
        } else {
            vec![AuthCredential::new("primary", token.trim())
                .map_err(|error| anyhow::anyhow!(error.message))?]
        };
        if let Ok(value) = env::var("GROKPTAH_SERVICE_CLIENTS") {
            client_credentials.extend(parse_client_credentials(&value)?);
        }
        let agent_owner_id = env::var("GROKPTAH_SERVICE_AGENT_OWNER")
            .unwrap_or_else(|_| "primary".into())
            .trim()
            .to_string();
        let workspace_value = env::var("GROKPTAH_SERVICE_WORKSPACES")
            .or_else(|_| env::var("GROKPTAH_CONTROL_WORKSPACES"))
            .unwrap_or_default();
        let workspaces = env::split_paths(&workspace_value).collect();
        let allow_remote = env_bool("GROKPTAH_SERVICE_ALLOW_REMOTE");
        let max_concurrent = env::var("GROKPTAH_SERVICE_MAX_CONCURRENT")
            .or_else(|_| env::var("GROKPTAH_CONTROL_MAX_CONCURRENT"))
            .ok()
            .map(|value| value.parse())
            .transpose()
            .context("GROKPTAH_SERVICE_MAX_CONCURRENT must be a positive integer")?
            .unwrap_or(DEFAULT_MAX_CONCURRENT);
        let timeout_ms = env::var("GROKPTAH_SERVICE_REQUEST_TIMEOUT_MS")
            .or_else(|_| env::var("GROKPTAH_CONTROL_REQUEST_TIMEOUT_MS"))
            .ok()
            .map(|value| value.parse())
            .transpose()
            .context("GROKPTAH_SERVICE_REQUEST_TIMEOUT_MS must be a positive integer")?
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_MS);
        Ok(Self {
            listen,
            token: token.trim().to_string(),
            workspaces,
            allow_remote,
            max_concurrent,
            request_timeout: Duration::from_millis(timeout_ms),
            client_credentials,
            agent_owner_id,
            runtime_home: None,
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.token.is_empty() {
            bail!("a bearer token is required; set --token or GROKPTAH_SERVICE_TOKEN");
        }
        if self.agent_owner_id.is_empty() || self.agent_owner_id.len() > 128 {
            bail!("GROKPTAH_SERVICE_AGENT_OWNER must be between 1 and 128 bytes");
        }
        if self.client_credentials.is_empty() {
            bail!("at least one service client credential is required");
        }
        let mut credential_ids = HashSet::new();
        let Some(primary) = self
            .client_credentials
            .iter()
            .find(|credential| credential.id == "primary")
        else {
            bail!("service client credentials must include the primary credential");
        };
        if primary.token() != self.token {
            bail!("the primary client credential must match the service token");
        }
        for credential in &self.client_credentials {
            if !credential_ids.insert(credential.id.as_str()) {
                bail!("duplicate service client credential id: {}", credential.id);
            }
            if !self.listen.ip().is_loopback() && credential.token().len() < 24 {
                bail!("remote listeners require every bearer token to be at least 24 characters");
            }
        }
        if self.workspaces.is_empty() {
            bail!(
                "at least one workspace is required; set --workspace or GROKPTAH_SERVICE_WORKSPACES"
            );
        }
        if self.max_concurrent == 0 {
            bail!("max concurrent requests must be greater than zero");
        }
        if self.request_timeout.is_zero() {
            bail!("request timeout must be greater than zero");
        }
        if !self.listen.ip().is_loopback() && !self.allow_remote {
            bail!(
                "non-loopback listeners require --allow-remote (health probes will require auth)"
            );
        }
        if !self.listen.ip().is_loopback() && self.token.len() < 24 {
            bail!("remote listeners require a bearer token of at least 24 characters");
        }
        Ok(())
    }

    /// Select a validated durable root without changing the legacy constructor
    /// or the environment-based desktop/service path.
    pub fn with_runtime_home(mut self, path: impl AsRef<std::path::Path>) -> Result<Self> {
        self.runtime_home = Some(RuntimeHome::from_path(path)?);
        Ok(self)
    }
}

pub enum StartupAction {
    Run(ServiceConfig),
    Help,
    Version,
}

pub fn parse_args<I, S>(args: I) -> Result<StartupAction>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut config = ServiceConfig::from_environment()?;
    let mut explicit_workspaces = false;
    let mut iter = args.into_iter().map(Into::into);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(StartupAction::Help),
            "--version" => return Ok(StartupAction::Version),
            "--listen" => {
                config.listen = next_value(&mut iter, "--listen")?
                    .parse()
                    .context("--listen must be an address such as 127.0.0.1:39200")?
            }
            "--token" => {
                config.token = next_value(&mut iter, "--token")?;
                let primary = AuthCredential::new("primary", config.token.clone())
                    .map_err(|error| anyhow::anyhow!(error.message))?;
                if let Some(existing) = config
                    .client_credentials
                    .iter_mut()
                    .find(|credential| credential.id == "primary")
                {
                    *existing = primary;
                } else {
                    config.client_credentials.push(primary);
                }
            }
            "--client" => config
                .client_credentials
                .push(parse_client_credential(&next_value(
                    &mut iter, "--client",
                )?)?),
            "--workspace" => {
                if !explicit_workspaces {
                    config.workspaces.clear();
                    explicit_workspaces = true;
                }
                config
                    .workspaces
                    .push(PathBuf::from(next_value(&mut iter, "--workspace")?));
            }
            "--allow-remote" => config.allow_remote = true,
            "--max-concurrent" => {
                config.max_concurrent = next_value(&mut iter, "--max-concurrent")?
                    .parse()
                    .context("--max-concurrent must be a positive integer")?;
            }
            "--request-timeout-ms" => {
                let value: u64 = next_value(&mut iter, "--request-timeout-ms")?
                    .parse()
                    .context("--request-timeout-ms must be a positive integer")?;
                config.request_timeout = Duration::from_millis(value);
            }
            value if value.starts_with('-') => bail!("unknown option {value}"),
            value => bail!("unexpected argument {value}"),
        }
    }
    config.validate()?;
    Ok(StartupAction::Run(config))
}

fn next_value<I>(iter: &mut I, flag: &str) -> Result<String>
where
    I: Iterator<Item = String>,
{
    iter.next()
        .filter(|value| !value.starts_with('-'))
        .with_context(|| format!("{flag} requires a value"))
}

fn env_bool(name: &str) -> bool {
    env::var(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parse_client_credential(spec: &str) -> Result<AuthCredential> {
    let (id, token) = spec
        .split_once('=')
        .with_context(|| "client credential must use ID=TOKEN format")?;
    AuthCredential::new(id, token).map_err(|error| anyhow::anyhow!(error.message))
}

fn parse_client_credentials(value: &str) -> Result<Vec<AuthCredential>> {
    value
        .split(',')
        .filter(|entry| !entry.trim().is_empty())
        .map(parse_client_credential)
        .collect()
}

pub fn help_text() -> &'static str {
    "GrokPtah headless service\n\nUsage: grokptah-service [options]\n\nOptions:\n  --listen ADDR                 Bind address (default 127.0.0.1:39200)\n  --token TOKEN                 Bearer token (or GROKPTAH_SERVICE_TOKEN)\n  --workspace PATH              Allowlisted workspace; repeatable\n  --client ID=TOKEN             Additional named device credential; repeatable\n  --allow-remote                Permit non-loopback bind; health requires auth\n  --max-concurrent N            Concurrent request/run ceiling (default 4)\n  --request-timeout-ms N        Request deadline (default 120000)\n  -h, --help                    Show this help\n      --version                 Show the service version\n\nGROKPTAH_SERVICE_CLIENTS accepts comma-separated ID=TOKEN entries.\nGROKPTAH_SERVICE_AGENT_OWNER names the durable Agent owner account.\nSet GROKPTAH_HOME to choose the durable service data directory."
}

pub struct ServiceHandle {
    pub addr: SocketAddr,
    pub token: String,
    /// The single non-cloneable owner of the process instance lock and of the
    /// task supervisor (#455). The attached control server is stopped and
    /// joined as step 2 of the runtime's ordered shutdown.
    runtime: HostRuntime,
}

impl ServiceHandle {
    /// Return a *request handle* on the process-owned host for embedding and
    /// service-level tests.
    ///
    /// The handle carries no process authority of its own: it can neither own
    /// nor release the instance lock, and it fails closed once the service has
    /// shut down. This accessor only exposes the shared host so callers can
    /// observe or seed durable state without opening a second ledger.
    pub fn host(&self) -> AgentHostHandle {
        self.runtime.handle()
    }

    /// Ordered shutdown: refuse new admissions, stop HTTP/SSE acceptance and
    /// join the serving task, cancel and join every supervised task, flush
    /// durable state, then release the instance lock exactly once. The lock
    /// file stays on disk.
    pub async fn stop_and_wait(self) -> HostShutdownReport {
        self.runtime.shutdown().await
    }
}

pub async fn start_service(config: ServiceConfig) -> Result<ServiceHandle> {
    config.validate()?;
    let allowlist = WorkspaceAllowlist::new(config.workspaces.clone());
    if allowlist.roots().len() != config.workspaces.len() {
        bail!("every configured workspace must exist and resolve to a directory");
    }

    let runtime = match config.runtime_home.clone() {
        Some(home) => AgentHost::create_with_runtime_home(HostConfig::default(), home),
        None => AgentHost::create(HostConfig::default()),
    };
    runtime.start().context("start GrokPtah agent host")?;
    let store: OrchStore = runtime
        .ensure_orchestration_store()
        .context("open durable orchestration store")?;
    let orch = OrchestrationService::new(
        runtime.handle(),
        runtime.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: config.token.clone(),
            allowlist,
            max_concurrent_runs: config.max_concurrent,
            bounds: Default::default(),
        },
    );
    orch.set_auth_credentials(config.client_credentials.clone())
        .map_err(|error| anyhow::anyhow!(error.message))?;
    orch.set_agent_owner_id(config.agent_owner_id.clone())
        .map_err(|error| anyhow::anyhow!(error.message))?;
    let limits = ControlServerLimits {
        max_concurrent: config.max_concurrent,
        request_timeout: config.request_timeout,
        inject_work_delay: None,
    };
    let server = start_control_server_with_bind(
        orch,
        config.listen,
        limits,
        !config.listen.ip().is_loopback(),
    )
    .await
    .context("bind GrokPtah service control plane")?;
    let addr = server.addr;
    runtime.attach_control_server(server);
    Ok(ServiceHandle {
        addr,
        token: config.token,
        runtime,
    })
}

pub async fn run_service(config: ServiceConfig) -> Result<()> {
    let handle = start_service(config).await?;
    eprintln!(
        "[grokptah-service] ready addr=http://{} health=http://{}/health",
        handle.addr, handle.addr
    );
    tokio::signal::ctrl_c()
        .await
        .context("wait for service shutdown signal")?;
    let report = handle.stop_and_wait().await;
    eprintln!(
        "[grokptah-service] stopped: joined {} supervised task(s), instance lock released={}",
        report.supervised_tasks_at_quiesce, report.process_lock_released
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_bind_requires_explicit_opt_in_and_strong_token() {
        let workspace = PathBuf::from("/tmp/project");
        assert!(ServiceConfig::new(
            "0.0.0.0:39200".parse().unwrap(),
            "a-strong-token-that-is-long-enough",
            vec![workspace.clone()],
            false,
            2,
            Duration::from_secs(10),
        )
        .is_err());
        assert!(ServiceConfig::new(
            "0.0.0.0:39200".parse().unwrap(),
            "short",
            vec![workspace],
            true,
            2,
            Duration::from_secs(10),
        )
        .is_err());
    }

    #[test]
    fn command_line_values_override_environment_defaults_without_mutating_env() {
        let action = parse_args([
            "--listen",
            "127.0.0.1:0",
            "--token",
            "cli-token",
            "--workspace",
            "/tmp/one",
            "--workspace",
            "/tmp/two",
            "--max-concurrent",
            "7",
            "--request-timeout-ms",
            "900",
        ])
        .unwrap();
        let StartupAction::Run(config) = action else {
            panic!("expected run action");
        };
        assert_eq!(config.listen, "127.0.0.1:0".parse().unwrap());
        assert_eq!(config.token, "cli-token");
        assert_eq!(
            config.workspaces,
            [PathBuf::from("/tmp/one"), PathBuf::from("/tmp/two")]
        );
        assert_eq!(config.max_concurrent, 7);
        assert_eq!(config.request_timeout, Duration::from_millis(900));
    }

    #[test]
    fn command_line_named_clients_are_parsed_and_duplicate_ids_fail_closed() {
        let action = parse_args([
            "--listen",
            "127.0.0.1:0",
            "--token",
            "primary-token",
            "--workspace",
            "/tmp/project",
            "--client",
            "laptop=secondary-token",
        ])
        .unwrap();
        let StartupAction::Run(config) = action else {
            panic!("expected run action");
        };
        assert!(config
            .client_credentials
            .iter()
            .any(|credential| credential.id == "laptop"));

        let mut duplicate = config;
        duplicate
            .client_credentials
            .push(AuthCredential::new("laptop", "another-token").unwrap());
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn explicit_runtime_home_is_validated_and_attached_without_changing_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let config = ServiceConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            "service-token",
            vec![PathBuf::from("/tmp/project")],
            false,
            2,
            Duration::from_secs(10),
        )
        .unwrap()
        .with_runtime_home(temp.path())
        .unwrap();
        assert_eq!(
            config.runtime_home.unwrap().path(),
            dunce::canonicalize(temp.path()).unwrap()
        );
    }
}
