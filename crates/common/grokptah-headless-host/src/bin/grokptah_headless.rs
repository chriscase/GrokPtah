//! The GrokPtah headless operator binary.
//!
//! `serve` runs the host in the foreground and speaks the NDJSON operator
//! protocol on stdin/stdout: one request per line in, one reply per line out,
//! diagnostics on stderr. There is no listening socket and no second writer, so
//! the host stays inside the authority boundary while still being steerable
//! while it runs.
//!
//! Stop it with `SIGTERM` or `Ctrl+C`: the first signal drains and checkpoints,
//! a second stops immediately and leaves the next start to recover.

use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

use clap::{Parser, Subcommand};
use grokptah_headless_host::clock::SystemClock;
use grokptah_headless_host::config::HostConfig;
use grokptah_headless_host::control::{ControlReply, MAX_REQUEST_BYTES};
use grokptah_headless_host::host::{HeadlessHost, engine_from_config};
use grokptah_headless_host::lifecycle::ShutdownKind;
use grokptah_headless_host::lock::HomeLock;
use grokptah_headless_host::{HostError, ShutdownSignal, signal};

/// Exit code for a refused or failed operation.
const EXIT_FAILURE: i32 = 1;
/// Default pause between engine steps while idle.
const DEFAULT_TICK_INTERVAL_MS: u64 = 250;

#[derive(Debug, Parser)]
#[command(
    name = "grokptah-headless",
    about = "Run durable GrokPtah runs without the desktop app",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Validate configuration and print the redacted effective settings.
    ConfigCheck {
        /// Path to the host configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Print host health. Also performs restart recovery on an unowned home.
    Health {
        /// Path to the host configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Print the capabilities this host can honor.
    Capabilities {
        /// Path to the host configuration file.
        #[arg(long)]
        config: PathBuf,
    },
    /// Run the host, reading NDJSON operator commands on stdin.
    Serve {
        /// Path to the host configuration file.
        #[arg(long)]
        config: PathBuf,
        /// Milliseconds between engine steps while idle.
        #[arg(long)]
        tick_interval_ms: Option<u64>,
        /// Stop after stdin closes instead of continuing to run.
        #[arg(long, default_value_t = false)]
        exit_on_eof: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = run(cli) {
        emit(&serde_json::json!({ "status": "error", "error": error.envelope() }));
        std::process::exit(EXIT_FAILURE);
    }
}

fn run(cli: Cli) -> Result<(), HostError> {
    match cli.command {
        Command::ConfigCheck { config } => {
            let config = load(&config)?;
            emit(&serde_json::json!({
                "status": "ok",
                "config": config.redacted_view(),
            }));
            Ok(())
        }
        Command::Health { config } => {
            let config = load(&config)?;
            if HomeLock::is_held(&config.home) {
                emit(&serde_json::json!({
                    "status": "ok",
                    "health": {
                        "contract": grokptah_headless_host::CONTRACT_VERSION,
                        "state": "starting",
                        "lockHeld": false,
                        "ownedElsewhere": true,
                        "sessionId": config.session_id,
                        "workspace": config.workspace_alias(),
                        "degraded": ["home_owned_elsewhere"],
                    },
                }));
                return Ok(());
            }
            let host = open(config)?;
            let report = host.startup_report();
            emit(&serde_json::json!({
                "status": "ok",
                "health": report.health,
                "recovery": report.recovery,
            }));
            Ok(())
        }
        Command::Capabilities { config } => {
            let config = load(&config)?;
            emit(&serde_json::json!({
                "status": "ok",
                "advertised": config.capabilities,
                "grants": config.grants,
            }));
            Ok(())
        }
        Command::Serve {
            config,
            tick_interval_ms,
            exit_on_eof,
        } => serve(
            load(&config)?,
            Duration::from_millis(tick_interval_ms.unwrap_or(DEFAULT_TICK_INTERVAL_MS).max(1)),
            exit_on_eof,
        ),
    }
}

fn load(path: &Path) -> Result<HostConfig, HostError> {
    let mut config = HostConfig::load(path)?;
    config.apply_overrides(|name| std::env::var(name).ok())?;
    Ok(config)
}

fn open(config: HostConfig) -> Result<HeadlessHost, HostError> {
    let engine = engine_from_config(&config)?;
    HeadlessHost::open(config, engine, Arc::new(SystemClock), ShutdownSignal::new())
}

/// Advance the host by one step, reporting a failure without stopping.
fn step(host: &mut HeadlessHost) {
    if let Err(error) = host.tick(1) {
        emit(&serde_json::json!({
            "status": "error",
            "event": "tick_failed",
            "error": error.envelope(),
        }));
    }
}

fn serve(config: HostConfig, tick_interval: Duration, exit_on_eof: bool) -> Result<(), HostError> {
    let engine = engine_from_config(&config)?;
    let shutdown = ShutdownSignal::new();
    let mut host = HeadlessHost::open(config, engine, Arc::new(SystemClock), shutdown.clone())?;

    // A host that cannot be stopped by a supervisor is worse than one that
    // fails to start, so a signal wiring failure is fatal rather than ignored.
    let _signals = signal::watch(shutdown.clone()).map_err(|error| {
        HostError::internal(
            "signal_wiring_failed",
            format!("stop signals could not be installed ({})", error.kind()),
        )
    })?;

    let report = host.startup_report();
    emit(&serde_json::json!({
        "status": "ok",
        "event": "started",
        "health": report.health,
        "recovery": report.recovery,
    }));

    let (sender, receiver) = channel::<Incoming>();
    std::thread::Builder::new()
        .name("grokptah-headless-stdin".to_owned())
        .spawn(move || read_requests(std::io::stdin().lock(), &sender))
        .map_err(|error| {
            HostError::internal(
                "stdin_reader_failed",
                format!("the control reader could not start ({})", error.kind()),
            )
        })?;

    let mut eof = false;
    loop {
        if shutdown.state() == ShutdownKind::Immediate {
            break;
        }

        if eof {
            // stdin is gone but the host keeps serving under its supervisor, so
            // idle on the tick interval rather than spinning on a dead channel.
            std::thread::sleep(tick_interval);
            step(&mut host);
        } else {
            match receiver.recv_timeout(tick_interval) {
                Ok(Incoming::Line(line)) if line.trim().is_empty() => {}
                Ok(Incoming::Line(line)) => emit_line(&host.handle_line(&line).to_line()),
                Ok(Incoming::TooLarge) => emit_line(
                    &ControlReply::error(
                        None,
                        &HostError::invalid(
                            "request_too_large",
                            "the request exceeds its byte bound",
                        ),
                    )
                    .to_line(),
                ),
                Err(RecvTimeoutError::Timeout) => step(&mut host),
                Err(RecvTimeoutError::Disconnected) => eof = true,
            }
        }

        if eof && exit_on_eof {
            shutdown.request(ShutdownKind::Graceful);
        }
        if shutdown.is_requested() {
            break;
        }
    }

    let kind = match shutdown.state() {
        ShutdownKind::None => ShutdownKind::Graceful,
        other => other,
    };
    let stop = host.shutdown(kind)?;
    emit(&serde_json::json!({
        "status": "ok",
        "event": "stopped",
        "kind": stop.kind.label(),
        "paused": stop.paused,
        "leftLive": stop.left_live,
    }));
    Ok(())
}

/// One item read from the operator's stream.
enum Incoming {
    /// A complete, bounded request line.
    Line(String),
    /// A line that exceeded the request bound and was discarded.
    TooLarge,
}

/// Read bounded NDJSON request lines until the stream ends.
///
/// A line is read through an explicit byte bound rather than `lines()`, so a
/// single unterminated write cannot make the host buffer without limit. An
/// over-long line is refused and skipped; the stream stays usable.
fn read_requests(mut reader: impl BufRead, sender: &Sender<Incoming>) {
    let mut line = Vec::new();
    loop {
        line.clear();
        let limit = MAX_REQUEST_BYTES as u64 + 1;
        let mut bounded = Read::take(reader.by_ref(), limit);
        let Ok(read) = bounded.read_until(b'\n', &mut line) else {
            return;
        };
        if read == 0 {
            return;
        }

        let terminated = line.last() == Some(&b'\n');
        let message = if !terminated && read > MAX_REQUEST_BYTES {
            let mut discard = Vec::new();
            if reader.read_until(b'\n', &mut discard).is_err() {
                return;
            }
            Incoming::TooLarge
        } else {
            Incoming::Line(String::from_utf8_lossy(&line).trim_end().to_owned())
        };
        if sender.send(message).is_err() {
            return;
        }
    }
}

fn emit(value: &serde_json::Value) {
    emit_line(&value.to_string());
}

fn emit_line(line: &str) {
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}
