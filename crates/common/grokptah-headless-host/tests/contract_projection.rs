//! Replay, redaction at the write boundary, and truthful projections.

mod common;

use common::{Harness, ok, phase, refused, submit};
use grokptah_headless_host::control::ControlCommand;
use grokptah_headless_host::redaction::REDACTED;

/// The whole on-disk footprint of one host home, as raw text.
fn home_contents(harness: &Harness) -> String {
    fn walk(dir: &std::path::Path, out: &mut String) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if let Ok(text) = std::fs::read_to_string(&path) {
                out.push_str(&text);
                out.push('\n');
            }
        }
    }
    let mut out = String::new();
    walk(harness.home.path(), &mut out);
    out
}

#[test]
fn a_cursor_outside_the_retained_window_is_told_to_recover() {
    let harness = Harness::new();
    let mut config = harness.config();
    config.limits.event_retention = 4;
    config.limits.max_rounds = 24;
    let mut host = harness.open_with(config);

    let run_id = submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(12) });

    let exact = ok(
        &mut host,
        ControlCommand::Events {
            run_id: run_id.clone(),
            after_seq: None,
            limit: Some(64),
        },
    );
    let start = exact["eventRange"]["startSeq"].as_u64().expect("start seq");
    assert!(start > 1, "old events must have been compacted away");

    let expired = ok(
        &mut host,
        ControlCommand::Events {
            run_id: run_id.clone(),
            after_seq: Some(1),
            limit: Some(64),
        },
    );
    assert_eq!(expired["page"]["cursorExpired"], true);
    assert_eq!(expired["recovery"]["kind"], "recovery");
    assert_eq!(expired["recovery"]["reason"], "cursor_expired");
    assert_eq!(expired["recovery"]["afterSeq"], 1);
    assert_eq!(expired["recovery"]["pollTool"], "events");
    assert_eq!(expired["recovery"]["scope"]["runId"], run_id.as_str());

    let ahead = ok(
        &mut host,
        ControlCommand::Events {
            run_id,
            after_seq: Some(10_000),
            limit: Some(64),
        },
    );
    assert_eq!(ahead["page"]["cursorExpired"], true);
    assert_eq!(ahead["recovery"]["reason"], "cursor_ahead");
    assert!(
        ahead["page"]["entries"]
            .as_array()
            .expect("entries")
            .is_empty()
    );
}

#[test]
fn paging_walks_the_retained_window_without_gaps_or_repeats() {
    let harness = Harness::new();
    let mut config = harness.config();
    config.limits.max_rounds = 24;
    let mut host = harness.open_with(config);

    let run_id = submit(&mut host, "req-1", "forever");
    ok(&mut host, ControlCommand::Tick { steps: Some(6) });

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = ok(
            &mut host,
            ControlCommand::Events {
                run_id: run_id.clone(),
                after_seq: cursor,
                limit: Some(2),
            },
        );
        assert_eq!(page["page"]["cursorExpired"], false);
        for entry in page["page"]["entries"].as_array().expect("entries") {
            seen.push(entry["seq"].as_u64().expect("seq"));
        }
        match page["page"]["nextCursor"].as_u64() {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    let expected: Vec<u64> = (1..=seen.len() as u64).collect();
    assert_eq!(seen, expected, "replay must be exact and in order");
}

#[test]
fn a_secret_in_a_prompt_never_reaches_disk_or_a_projection() {
    let harness = Harness::new();
    let mut host = harness.open();

    let secret = "xai-abcdefghijklmnopqrstuvwxyz012345";
    let workspace = harness.workspace.path().display().to_string();
    let prompt = format!("build in {workspace} using XAI_API_KEY={secret}");
    let run_id = submit(&mut host, "req-1", &prompt);

    let preview = common::status(&mut host, &run_id)["durable"]["promptPreview"]
        .as_str()
        .expect("prompt preview")
        .to_owned();
    assert!(!preview.contains(secret), "preview leaked the credential");
    assert!(
        !preview.contains(&workspace),
        "preview leaked the host path"
    );
    assert!(preview.contains("<workspace>"));
    assert!(preview.contains(REDACTED));

    let on_disk = home_contents(&harness);
    assert!(
        !on_disk.contains(secret),
        "the credential reached durable storage"
    );
    assert!(
        !on_disk.contains(&workspace),
        "a host path reached durable storage"
    );
}

#[test]
fn an_engine_detail_is_scrubbed_before_it_is_journaled() {
    let harness = Harness::new();
    let mut host = harness.open();

    let run_id = submit(&mut host, "req-leak", "leak");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });
    assert_eq!(phase(&mut host, &run_id), "failed");

    let events = ok(
        &mut host,
        ControlCommand::Events {
            run_id,
            after_seq: None,
            limit: Some(64),
        },
    );
    let rendered = events.to_string();
    assert!(rendered.contains("engine_leak"), "the reason must survive");
    assert!(
        !rendered.contains("abcdefghijklmnopqrstuvwxyz"),
        "the credential must not survive"
    );
    assert!(!home_contents(&harness).contains("abcdefghijklmnopqrstuvwxyz"));
}

#[test]
fn a_completion_that_names_a_path_outside_the_workspace_is_refused() {
    let harness = Harness::new();
    let mut host = harness.open();

    let run_id = submit(&mut host, "req-escape", "escape");
    ok(&mut host, ControlCommand::Tick { steps: Some(2) });

    let status = common::status(&mut host, &run_id);
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["stopReason"], "completion_path_rejected");
    assert_eq!(status["receiptAvailable"], false);
    assert_eq!(
        refused(&mut host, ControlCommand::Receipt { run_id }),
        "receipt_absent"
    );
    assert!(!home_contents(&harness).contains("/etc/shadow"));
}

#[test]
fn a_malformed_request_is_answered_not_swallowed() {
    let harness = Harness::new();
    let mut host = harness.open();

    let reply = host.handle_line("{\"command\":{\"op\":\"nope\"}}");
    assert!(!reply.is_ok());
    assert!(reply.to_line().contains("request_malformed"));

    let reply = host.handle_line("not json at all");
    assert!(!reply.is_ok());

    // The host is still usable afterwards.
    let health = ok(&mut host, ControlCommand::Health);
    assert_eq!(health["state"], "ready");
}

#[test]
fn capabilities_report_only_what_the_host_can_actually_honor() {
    let harness = Harness::new();
    let mut host = harness.open();

    let payload = ok(&mut host, ControlCommand::Capabilities);
    let permitted: Vec<&str> = payload["permitted"]
        .as_array()
        .expect("permitted")
        .iter()
        .map(|value| value.as_str().expect("capability id"))
        .collect();

    assert!(permitted.contains(&"session.observe"));
    assert!(permitted.contains(&"run.execute"));
    assert!(
        !permitted.contains(&"run.promote"),
        "a gated capability without a grant is not permitted"
    );
    assert_eq!(
        payload["advertised"]["contract"],
        grokptah_headless_host::CONTRACT_VERSION
    );
}
