//! Shared offline harness for the headless host contract tests.
//!
//! Everything is synthetic: temporary directories, a fixed clock, and the
//! scripted fixture engine. No provider credential, no network, no real
//! workspace, and no wall-clock sleep.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use grokptah_agent_sdk::CapabilityAvailability;
use grokptah_headless_host::authority::CAP_RESUME;
use grokptah_headless_host::clock::FixedClock;
use grokptah_headless_host::config::{EngineSelection, HostConfig};
use grokptah_headless_host::control::{
    ControlCommand, ControlReply, ControlRequest, ControlResult,
};
use grokptah_headless_host::engine::RunEngine;
use grokptah_headless_host::host::{HeadlessHost, engine_from_config};
use grokptah_headless_host::lifecycle::ShutdownSignal;
use grokptah_headless_host::orchestration::OrchestratedEngine;
use grokptah_headless_host::testing;
use grokptah_headless_host::testing::{DispatchLog, FakeOrchestrator, FakeTurn};
use serde_json::Value;
use tempfile::TempDir;

/// Directory name of the harness workspace, and therefore its bound alias.
pub const WORKSPACE_NAME: &str = "project";

/// A disposable host home, workspace, fixture script, and clock.
pub struct Harness {
    pub home: TempDir,
    pub workspace: TempDir,
    pub script: PathBuf,
    pub clock: Arc<FixedClock>,
    pub shutdown: ShutdownSignal,
    grants: Vec<String>,
    engine_enabled: bool,
}

impl Harness {
    /// Build a harness whose capabilities are all available except the gated
    /// promote capability, which stays gated and ungranted by default.
    pub fn new() -> Self {
        // A stable, bindable directory name: the workspace alias is what an
        // orchestrator binds to, and a random temporary name is not one.
        let workspace = TempDir::new().expect("workspace");
        let root = workspace.path().join(WORKSPACE_NAME);
        std::fs::create_dir_all(&root).expect("workspace root");
        let script = root.join("fixture-script.json");
        std::fs::write(&script, testing::FIXTURE_SCRIPT).expect("write fixture script");
        Self {
            home: TempDir::new().expect("home"),
            workspace,
            script,
            clock: Arc::new(FixedClock::new(testing::NOW_MS)),
            shutdown: ShutdownSignal::new(),
            grants: Vec::new(),
            engine_enabled: true,
        }
    }

    /// The approved workspace root. Its directory name is the bound alias.
    pub fn workspace_path(&self) -> PathBuf {
        self.workspace.path().join(WORKSPACE_NAME)
    }

    /// Record an explicit operator grant for a gated capability.
    pub fn grant(mut self, capability_id: &str) -> Self {
        self.grants.push(capability_id.to_owned());
        self
    }

    /// Configure the host with no run engine at all.
    pub fn without_engine(mut self) -> Self {
        self.engine_enabled = false;
        self
    }

    /// Build a validated configuration pointing at this harness.
    pub fn config(&self) -> HostConfig {
        let mut config = testing::config_for(self.home.path(), &self.workspace_path());
        for descriptor in &mut config.capabilities.capabilities {
            if descriptor.id == CAP_RESUME {
                descriptor.availability = CapabilityAvailability::Available;
            }
        }
        config.grants = self.grants.clone();
        config.engine = if self.engine_enabled {
            EngineSelection::Fixture {
                script: self.script.clone(),
            }
        } else {
            EngineSelection::Disabled
        };
        config
    }

    /// Open a host over this harness. Dropping the host releases the home lock.
    pub fn open(&self) -> HeadlessHost {
        self.open_with(self.config())
    }

    /// Open a host over an adjusted configuration for this harness.
    pub fn open_with(&self, config: HostConfig) -> HeadlessHost {
        let engine = engine_from_config(&config).expect("engine builds");
        HeadlessHost::open(config, engine, self.clock.clone(), self.shutdown.clone())
            .expect("host opens")
    }

    /// Open a host driving an injected engine instead of a configured one.
    ///
    /// This is the shape a real deployment uses: the engine is handed in, so
    /// the host never has to know how a turn is produced.
    pub fn open_injected(&self, engine: Box<dyn RunEngine>) -> HeadlessHost {
        let mut config = self.config();
        config.engine = EngineSelection::Disabled;
        HeadlessHost::open(
            config,
            Some(engine),
            self.clock.clone(),
            self.shutdown.clone(),
        )
        .expect("host opens")
    }

    /// Open a host driven by a scripted fake orchestrator through the adapter.
    ///
    /// Returns the shared dispatch log so a test can assert exactly which turns
    /// the orchestrator was asked to run.
    pub fn open_orchestrated(&self, turns: Vec<FakeTurn>) -> (HeadlessHost, DispatchLog) {
        let log = DispatchLog::new();
        let engine = OrchestratedEngine::new(FakeOrchestrator::fixture(log.clone(), turns));
        (self.open_injected(Box::new(engine)), log)
    }

    /// Try to open a host, surfacing the refusal instead of panicking.
    pub fn try_open(&self) -> Result<HeadlessHost, grokptah_headless_host::HostError> {
        let config = self.config();
        let engine = engine_from_config(&config)?;
        HeadlessHost::open(config, engine, self.clock.clone(), self.shutdown.clone())
    }
}

/// Send one command and return the reply.
pub fn send(host: &mut HeadlessHost, command: ControlCommand) -> ControlReply {
    host.handle(ControlRequest {
        id: Some("t".to_owned()),
        command,
    })
}

/// Send one command and require success, returning the payload.
pub fn ok(host: &mut HeadlessHost, command: ControlCommand) -> Value {
    let label = command.label();
    match send(host, command).result {
        ControlResult::Ok { payload } => payload,
        ControlResult::Error { error } => {
            panic!(
                "{label} was refused: {:?} {}",
                error.reason_code, error.message
            )
        }
    }
}

/// Send one command and require refusal, returning the stable reason code.
pub fn refused(host: &mut HeadlessHost, command: ControlCommand) -> String {
    let label = command.label();
    match send(host, command).result {
        ControlResult::Ok { payload } => panic!("{label} unexpectedly succeeded: {payload}"),
        ControlResult::Error { error } => error
            .reason_code
            .expect("every host refusal carries a stable reason code"),
    }
}

/// Submit the named fixture prompt and return the created run identity.
pub fn submit(host: &mut HeadlessHost, request_id: &str, prompt: &str) -> String {
    let payload = ok(
        host,
        ControlCommand::Submit {
            request_id: request_id.to_owned(),
            prompt: prompt.to_owned(),
            bounds: None,
            execution_mode: None,
            allow_queue: Some(true),
        },
    );
    payload["run"]["durable"]["runId"]
        .as_str()
        .expect("submit returns a run identity")
        .to_owned()
}

/// Read one run's status payload.
pub fn status(host: &mut HeadlessHost, run_id: &str) -> Value {
    ok(
        host,
        ControlCommand::Status {
            run_id: run_id.to_owned(),
        },
    )
}

/// Current host phase for a run.
pub fn phase(host: &mut HeadlessHost, run_id: &str) -> String {
    status(host, run_id)["phase"]
        .as_str()
        .expect("status carries the exact phase")
        .to_owned()
}

/// Current revision for a run.
pub fn revision(host: &mut HeadlessHost, run_id: &str) -> u64 {
    status(host, run_id)["revision"]
        .as_u64()
        .expect("status carries the revision")
}
