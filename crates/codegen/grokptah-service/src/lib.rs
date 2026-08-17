//! Standalone headless GrokPtah service.
//!
//! The service owns process startup and configuration only. Agent execution,
//! durable orchestration, MCP authorization, and event recovery remain in the
//! shared `grokptah-agent-bridge` crate used by the desktop client.

use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    start_control_server_with_bind, AgentHost, AgentHostHandle, ControlServerHandle,
    ControlServerLimits, HostConfig, OrchStore, OrchestrationConfig, OrchestrationService,
    WorkspaceAllowlist,
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
        let config = Self {
            listen,
            token: token.into().trim().to_string(),
            workspaces,
            allow_remote,
            max_concurrent,
            request_timeout,
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
        })
    }

    pub fn validate(&self) -> Result<()> {
        if self.token.is_empty() {
            bail!("a bearer token is required; set --token or GROKPTAH_SERVICE_TOKEN");
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
            "--token" => config.token = next_value(&mut iter, "--token")?,
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

pub fn help_text() -> &'static str {
    "GrokPtah headless service\n\nUsage: grokptah-service [options]\n\nOptions:\n  --listen ADDR                 Bind address (default 127.0.0.1:39200)\n  --token TOKEN                 Bearer token (or GROKPTAH_SERVICE_TOKEN)\n  --workspace PATH              Allowlisted workspace; repeatable\n  --allow-remote                Permit non-loopback bind; health requires auth\n  --max-concurrent N            Concurrent request/run ceiling (default 4)\n  --request-timeout-ms N        Request deadline (default 120000)\n  -h, --help                    Show this help\n      --version                 Show the service version\n\nSet GROKPTAH_HOME to choose the durable service data directory."
}

pub struct ServiceHandle {
    pub addr: SocketAddr,
    pub token: String,
    server: Option<ControlServerHandle>,
    host: AgentHostHandle,
}

impl ServiceHandle {
    /// Return the process-owned host for embedding and service-level tests.
    ///
    /// The handle remains responsible for stopping the server and host; this
    /// accessor only exposes the shared host so callers can observe or seed
    /// durable state without opening a second orchestration ledger.
    pub fn host(&self) -> AgentHostHandle {
        self.host.clone()
    }

    pub async fn stop_and_wait(mut self) {
        if let Some(server) = self.server.take() {
            server.stop_and_wait().await;
        }
        let _ = self.host.stop();
    }
}

pub async fn start_service(config: ServiceConfig) -> Result<ServiceHandle> {
    config.validate()?;
    let allowlist = WorkspaceAllowlist::new(config.workspaces.clone());
    if allowlist.roots().len() != config.workspaces.len() {
        bail!("every configured workspace must exist and resolve to a directory");
    }

    let host = AgentHost::create(HostConfig::default());
    host.start().context("start GrokPtah agent host")?;
    let store: OrchStore = host
        .ensure_orchestration_store()
        .context("open durable orchestration store")?;
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store,
        OrchestrationConfig {
            bearer_token: config.token.clone(),
            allowlist,
            max_concurrent_runs: config.max_concurrent,
            bounds: Default::default(),
        },
    );
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
    Ok(ServiceHandle {
        addr: server.addr,
        token: config.token,
        server: Some(server),
        host,
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
    handle.stop_and_wait().await;
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
}
