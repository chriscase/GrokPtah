//! The operator binary: startup, config validation, and the NDJSON protocol.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use common::Harness;
use grokptah_headless_host::config::HostConfig;
use serde_json::Value;

/// Path to the binary under test, provided by Cargo.
const BINARY: &str = env!("CARGO_BIN_EXE_grokptah-headless");

fn write_config(dir: &Path, config: &HostConfig) -> std::path::PathBuf {
    let path = dir.join("headless.json");
    std::fs::write(
        &path,
        serde_json::to_string_pretty(config).expect("config serializes"),
    )
    .expect("write config");
    path
}

fn run(args: &[&str]) -> (bool, Vec<Value>) {
    let output = Command::new(BINARY)
        .args(args)
        .output()
        .expect("binary runs");
    (output.status.success(), parse_lines(&output.stdout))
}

fn parse_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("every stdout line is one JSON object"))
        .collect()
}

#[test]
fn config_check_validates_and_prints_only_share_safe_settings() {
    let harness = Harness::new();
    let config = harness.config();
    let path = write_config(harness.workspace.path(), &config);

    let (success, lines) = run(&["config-check", "--config", path.to_str().expect("utf-8")]);
    assert!(success, "a valid configuration must be accepted");
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["status"], "ok");

    let rendered = lines[0].to_string();
    assert_eq!(lines[0]["config"]["home"], "<home>");
    assert_eq!(lines[0]["config"]["workspace"], "<workspace>");
    assert!(
        !rendered.contains(harness.home.path().to_str().expect("utf-8")),
        "config-check must not echo the host home path"
    );
    assert_eq!(lines[0]["config"]["engine"], "fixture");
}

#[test]
fn a_configuration_that_would_share_the_desktop_home_is_refused() {
    let harness = Harness::new();
    let mut config = harness.config();
    config.home = harness.home.path().join(".grokptah");
    let path = write_config(harness.workspace.path(), &config);

    let (success, lines) = run(&["config-check", "--config", path.to_str().expect("utf-8")]);
    assert!(!success, "the desktop home must be refused");
    assert_eq!(lines[0]["error"]["reasonCode"], "desktop_home_refused");
    assert_eq!(lines[0]["error"]["code"], "invalid_request");
}

#[test]
fn an_unknown_configuration_key_is_refused_rather_than_ignored() {
    let harness = Harness::new();
    let mut raw: Value = serde_json::to_value(harness.config()).expect("config serializes");
    raw["allowEverything"] = Value::Bool(true);
    let path = harness.workspace.path().join("bad.json");
    std::fs::write(&path, raw.to_string()).expect("write config");

    let (success, lines) = run(&["config-check", "--config", path.to_str().expect("utf-8")]);
    assert!(!success);
    assert_eq!(lines[0]["error"]["reasonCode"], "config_malformed");
}

#[test]
fn health_reports_a_home_that_another_host_already_owns() {
    let harness = Harness::new();
    let path = write_config(harness.workspace.path(), &harness.config());
    let args = ["health", "--config", path.to_str().expect("utf-8")];

    let (success, lines) = run(&args);
    assert!(success);
    assert_eq!(lines[0]["health"]["state"], "ready");
    assert_eq!(lines[0]["health"]["lockHeld"], true);

    let _owner = harness.open();
    let (success, lines) = run(&args);
    assert!(success);
    assert_eq!(lines[0]["health"]["ownedElsewhere"], true);
    assert_eq!(lines[0]["health"]["lockHeld"], false);
    assert!(
        lines[0]["health"]["degraded"]
            .as_array()
            .expect("degraded")
            .contains(&serde_json::json!("home_owned_elsewhere"))
    );
}

#[test]
fn serve_drives_a_run_to_a_receipt_over_ndjson_and_stops_cleanly() {
    let harness = Harness::new();
    let path = write_config(harness.workspace.path(), &harness.config());

    let requests = [
        r#"{"id":"1","command":{"op":"submit","requestId":"req-1","prompt":"build","allowQueue":true}}"#,
        r#"{"id":"2","command":{"op":"tick","steps":4}}"#,
        r#"{"id":"3","command":{"op":"status","runId":"RUN"}}"#,
        r#"{"id":"4","command":{"op":"receipt","runId":"RUN"}}"#,
    ];

    // The run identity is derived, so the first reply names it.
    let mut child = Command::new(BINARY)
        .args([
            "serve",
            "--config",
            path.to_str().expect("utf-8"),
            // A long idle interval keeps the run advancing only on explicit
            // ticks, so the transcript is deterministic.
            "--tick-interval-ms",
            "60000",
            "--exit-on-eof",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve starts");

    let run_id = grokptah_headless_host::identity::opaque_id(
        "run",
        &[
            &harness.config().session_id,
            &harness.config().workspace_alias(),
            "req-1",
        ],
    );

    {
        let mut stdin = child.stdin.take().expect("stdin");
        for request in requests {
            let line = request.replace("RUN", &run_id);
            writeln!(stdin, "{line}").expect("write request");
        }
    }

    let output = child.wait_with_output().expect("serve exits");
    assert!(
        output.status.success(),
        "serve exited with {:?}",
        output.status
    );
    let lines = parse_lines(&output.stdout);

    assert_eq!(lines[0]["event"], "started");
    assert_eq!(lines[0]["health"]["state"], "ready");

    let replies: Vec<&Value> = lines
        .iter()
        .filter(|line| line.get("id").is_some())
        .collect();
    assert_eq!(replies.len(), 4, "one reply per request, in order");
    assert_eq!(replies[0]["id"], "1");
    assert_eq!(replies[0]["result"]["status"], "ok");
    assert_eq!(
        replies[0]["result"]["payload"]["run"]["durable"]["runId"],
        run_id.as_str()
    );
    assert_eq!(replies[2]["result"]["payload"]["phase"], "completed");
    assert_eq!(
        replies[3]["result"]["payload"]["fingerprint"],
        "fingerprint-build"
    );

    let stopped = lines.last().expect("a stop line");
    assert_eq!(stopped["event"], "stopped");
    assert_eq!(stopped["kind"], "graceful");
}

#[test]
fn serve_refuses_a_request_without_dropping_the_connection() {
    let harness = Harness::new();
    let path = write_config(harness.workspace.path(), &harness.config());

    let mut child = Command::new(BINARY)
        .args([
            "serve",
            "--config",
            path.to_str().expect("utf-8"),
            "--tick-interval-ms",
            "60000",
            "--exit-on-eof",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve starts");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, "{{\"id\":\"1\",\"command\":{{\"op\":\"nope\"}}}}").expect("write");
        writeln!(stdin, "not json").expect("write");
        writeln!(stdin, "{{\"id\":\"3\",\"command\":{{\"op\":\"health\"}}}}").expect("write");
    }

    let output = child.wait_with_output().expect("serve exits");
    assert!(output.status.success());
    let replies: Vec<Value> = parse_lines(&output.stdout)
        .into_iter()
        .filter(|line| line.get("result").is_some())
        .collect();

    assert_eq!(replies.len(), 3);
    assert_eq!(
        replies[0]["result"]["error"]["reasonCode"],
        "request_malformed"
    );
    assert_eq!(
        replies[1]["result"]["error"]["reasonCode"],
        "request_malformed"
    );
    assert_eq!(replies[2]["result"]["status"], "ok");
    assert_eq!(replies[2]["result"]["payload"]["state"], "ready");
}

#[test]
fn an_oversized_request_line_is_refused_without_buffering_it() {
    let harness = Harness::new();
    let path = write_config(harness.workspace.path(), &harness.config());

    let mut child = Command::new(BINARY)
        .args([
            "serve",
            "--config",
            path.to_str().expect("utf-8"),
            "--tick-interval-ms",
            "60000",
            "--exit-on-eof",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve starts");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        let oversized = "x".repeat(grokptah_headless_host::control::MAX_REQUEST_BYTES + 64);
        writeln!(stdin, "{oversized}").expect("write");
        // The stream stays usable after the over-long line is discarded.
        writeln!(stdin, r#"{{"id":"2","command":{{"op":"health"}}}}"#).expect("write");
    }

    let output = child.wait_with_output().expect("serve exits");
    assert!(output.status.success());
    let replies: Vec<Value> = parse_lines(&output.stdout)
        .into_iter()
        .filter(|line| line.get("result").is_some())
        .collect();

    assert_eq!(replies.len(), 2);
    assert_eq!(
        replies[0]["result"]["error"]["reasonCode"],
        "request_too_large"
    );
    assert_eq!(replies[1]["id"], "2");
    assert_eq!(replies[1]["result"]["status"], "ok");
}

#[test]
fn a_shutdown_request_stops_the_host_and_checkpoints_live_runs() {
    let harness = Harness::new();
    let path = write_config(harness.workspace.path(), &harness.config());

    let mut child = Command::new(BINARY)
        .args([
            "serve",
            "--config",
            path.to_str().expect("utf-8"),
            "--tick-interval-ms",
            "60000",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("serve starts");

    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(
            stdin,
            r#"{{"id":"1","command":{{"op":"submit","requestId":"req-1","prompt":"forever","allowQueue":true}}}}"#
        )
        .expect("write");
        writeln!(stdin, r#"{{"id":"2","command":{{"op":"tick","steps":1}}}}"#).expect("write");
        writeln!(stdin, r#"{{"id":"3","command":{{"op":"shutdown"}}}}"#).expect("write");
    }

    let output = child.wait_with_output().expect("serve exits");
    assert!(output.status.success());
    let lines = parse_lines(&output.stdout);
    let stopped = lines.last().expect("a stop line");
    assert_eq!(stopped["event"], "stopped");
    assert_eq!(stopped["kind"], "graceful");
    assert_eq!(
        stopped["paused"].as_array().expect("paused").len(),
        1,
        "a live run must be checkpointed by a graceful stop"
    );
}
