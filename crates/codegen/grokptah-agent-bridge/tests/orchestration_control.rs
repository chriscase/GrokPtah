//! Integration tests for #196 orchestration control plane.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    hash_payload, AgentModelSpec, OrchStore, OrchestrationConfig, OrchestrationService, RunBounds,
    RunExecutionMode, RunRecord, RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    discovered_tool_names, model_selection_key, set_grokptah_home_override, start_control_server,
    AgentHost, EventBus, HostConfig, SessionKind, SessionUpdate, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

use common::ProcessEnvGuard;

/// Serializes home-override + instance-lock across tests (same as bridge lifecycle tests).
fn setup_home() -> (tempfile::TempDir, ProcessEnvGuard) {
    let mut guard = ProcessEnvGuard::new();
    let d = tempdir().unwrap();
    let home = d.path().join(".grokptah");
    std::fs::create_dir_all(&home).unwrap();
    set_grokptah_home_override(Some(home));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");
    (d, guard)
}

fn started_host() -> grokptah_agent_bridge::AgentHostHandle {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("start host");
    host
}

/// S2: `PromptQueueChanged` is published *after* the mutation lock is
/// released, so the bus `seq` reflects publish order, not commit order. The
/// per-session `revision` is stamped under the mutation lock instead, which
/// means sorting snapshots by revision must reproduce the order the queue
/// actually changed in.
///
/// Concretely: N concurrent adds must yield revisions 1..=N whose snapshots
/// grow 1, 2, ..., N. Stamping the revision at publish time (or reusing the
/// bus seq) pairs a late revision with an early snapshot and fails here.
#[test]
fn queue_revision_orders_snapshots_by_commit_not_publish() {
    let (_home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();

    const WRITERS: usize = 8;
    let mut events = host.event_bus().subscribe();

    let handles: Vec<_> = (0..WRITERS)
        .map(|i| {
            let host = host.clone();
            std::thread::spawn(move || {
                host.session_queue_add(session.id, format!("prompt {i}"), false)
                    .expect("queue add");
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("writer thread");
    }
    // A second session's counter is independent, and its events must not
    // perturb the first session's watermark.
    host.session_queue_add(other.id, "unrelated".into(), false)
        .unwrap();

    let mut observed: Vec<(u64, usize)> = Vec::new();
    let mut other_revisions: Vec<u64> = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let SessionUpdate::PromptQueueChanged {
            session_id,
            revision,
            entries,
            ..
        } = event
        {
            if session_id == session.id {
                observed.push((revision, entries.len()));
            } else if session_id == other.id {
                other_revisions.push(revision);
            }
        }
    }

    assert_eq!(observed.len(), WRITERS, "one snapshot per committed add");
    observed.sort_by_key(|(revision, _)| *revision);
    assert_eq!(
        observed.iter().map(|(r, _)| *r).collect::<Vec<_>>(),
        (1..=WRITERS as u64).collect::<Vec<_>>(),
        "revisions must be dense and monotonic per session"
    );
    assert_eq!(
        observed.iter().map(|(_, len)| *len).collect::<Vec<_>>(),
        (1..=WRITERS).collect::<Vec<_>>(),
        "revision order must match the order the queue actually grew in"
    );
    assert_eq!(
        other_revisions,
        vec![1],
        "each session carries its own revision counter"
    );
    assert_eq!(
        host.session_queue_list(session.id).unwrap().len(),
        WRITERS,
        "every concurrent add must survive"
    );
    set_grokptah_home_override(None);
}

/// Every queue mutation kind has to stamp a revision, or a GUI holding a
/// watermark would ignore that action's snapshot forever.
#[test]
fn every_queue_mutation_advances_the_revision() {
    let (_home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let mut events = host.event_bus().subscribe();

    let first = host
        .session_queue_add(session.id, "one".into(), false)
        .unwrap()[0]
        .clone();
    host.session_queue_add(session.id, "two".into(), false)
        .unwrap();
    // Every mutator is compare-and-set now, and reorder bumps the versions it
    // shifts, so each step has to re-read the version the previous one left.
    let version_of = |entry_id: &str| {
        host.session_queue_list(session.id)
            .unwrap()
            .into_iter()
            .find(|entry| entry.id == entry_id)
            .expect("entry present")
            .version
    };
    host.session_queue_edit(session.id, &first.id, first.version, "one edited".into())
        .unwrap();
    host.session_queue_move(
        session.id,
        &first.id,
        1,
        version_of(&first.id),
        host.session_queue_snapshot(session.id).unwrap().revision,
    )
    .unwrap();
    host.session_queue_run_next(session.id, &first.id, version_of(&first.id))
        .unwrap();
    host.session_queue_remove(session.id, &first.id, version_of(&first.id))
        .unwrap();
    host.session_queue_clear(session.id).unwrap();

    let mut revisions = Vec::new();
    let mut actions = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let SessionUpdate::PromptQueueChanged {
            revision, action, ..
        } = event
        {
            revisions.push(revision);
            actions.push(action);
        }
    }
    assert_eq!(
        actions,
        vec![
            "queued",
            "queued",
            "edited",
            "reordered",
            "run_next",
            "removed",
            "cleared"
        ]
    );
    assert_eq!(revisions, (1..=7).collect::<Vec<u64>>());
    set_grokptah_home_override(None);
}

#[test]
fn dual_subscriber_same_ordered_sequences() {
    let (_home, _lock) = setup_home();
    let host = started_host();
    let bus = host.event_bus();
    let mut gui = bus.subscribe();
    let mut mcp = bus.subscribe();
    let sid = Uuid::new_v4();
    for i in 0..10 {
        bus.publish(SessionUpdate::AgentMessageChunk {
            session_id: sid,
            text: format!("x{i}"),
        });
    }
    for i in 0..10 {
        let a = gui.try_recv().unwrap();
        let b = mcp.try_recv().unwrap();
        match (a, b) {
            (
                SessionUpdate::AgentMessageChunk { text: ta, .. },
                SessionUpdate::AgentMessageChunk { text: tb, .. },
            ) => {
                assert_eq!(ta, format!("x{i}"));
                assert_eq!(tb, ta);
            }
            _ => panic!("variant"),
        }
    }
    // journal seq monotonic
    let page = bus.read_after(0, 100);
    assert!(!page.cursor_expired);
    let mut last = 0u64;
    for e in &page.entries {
        assert!(e.seq > last);
        last = e.seq;
    }
    set_grokptah_home_override(None);
}

#[test]
fn schema_snapshot_excludes_forbidden() {
    let names = discovered_tool_names();
    for t in CONTROL_TOOLS {
        assert!(names.contains(t), "missing {t}");
    }
    for f in FORBIDDEN_TOOLS {
        assert!(!names.contains(f), "forbidden {f}");
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn idempotency_conflict_and_replay() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();

    let bus = host.event_bus();
    let store = OrchStore::open(home.path().join("orch")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        bus,
        store,
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let r1 = orch
        .queue_prompt(
            &auth,
            "req-1",
            session.id,
            ws.path(),
            "hello world".into(),
            false,
        )
        .await
        .unwrap();
    let r2 = orch
        .queue_prompt(
            &auth,
            "req-1",
            session.id,
            ws.path(),
            "hello world".into(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(r1, r2);
    let conflict = orch.queue_prompt(
        &auth,
        "req-1",
        session.id,
        ws.path(),
        "different payload".into(),
        false,
    );
    assert!(conflict.await.is_err());
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn mcp_queue_controls_are_scoped_versioned_and_replay_safe() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let first = orch
        .queue_prompt(
            &auth,
            "queue-seed-1",
            session.id,
            ws.path(),
            "first queued prompt".into(),
            false,
        )
        .await
        .unwrap();
    let first_entry = first["entries"][0].clone();
    let first_id = first_entry["id"].as_str().unwrap();
    let first_version = first_entry["version"].as_u64().unwrap();
    let mut events = host.event_bus().subscribe();

    let edited = orch
        .edit_queue(
            &auth,
            "queue-edit-1",
            session.id,
            ws.path(),
            first_id,
            first_version,
            "edited queued prompt".into(),
        )
        .await
        .unwrap();
    assert_eq!(edited["actionId"], "queue-edit-1");
    assert_eq!(edited["origin"], "mcp");
    assert_eq!(edited["entry"]["version"], 1);

    let replay = orch
        .edit_queue(
            &auth,
            "queue-edit-1",
            session.id,
            ws.path(),
            first_id,
            first_version,
            "edited queued prompt".into(),
        )
        .await
        .unwrap();
    assert_eq!(replay, edited);
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), 1);

    let stale = orch
        .edit_queue(
            &auth,
            "queue-edit-stale",
            session.id,
            ws.path(),
            first_id,
            first_version,
            "must not apply".into(),
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code.as_str(), "stale_version");
    assert_eq!(
        host.session_queue_list(session.id).unwrap()[0].text,
        "edited queued prompt"
    );

    let event = events.try_recv().expect("queue mutation event");
    match event {
        SessionUpdate::PromptQueueChanged {
            action,
            origin,
            changed_entry: Some(entry),
            ..
        } => {
            assert_eq!(action, "edited");
            assert_eq!(origin, "mcp");
            assert_eq!(entry.text, "edited queued prompt");
        }
        other => panic!("unexpected queue event: {other:?}"),
    }

    let listed = orch.get_queue(&auth, session.id, ws.path()).unwrap();
    assert_eq!(listed["entries"].as_array().unwrap().len(), 1);
    let steered = orch
        .steer_queued(&auth, "queue-steer-1", session.id, ws.path(), first_id, 1)
        .await
        .unwrap();
    assert_eq!(steered["action"], "steer_now");
    assert_eq!(steered["disposition"], "queued");
    assert_eq!(steered["entry"]["source"], "steering_deferred");
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), 1);

    let outside = tempdir().unwrap();
    let denied = orch
        .get_queue(&auth, session.id, outside.path())
        .unwrap_err();
    assert_eq!(denied.code.as_str(), "workspace_mismatch");
    set_grokptah_home_override(None);
}

/// S3: the desktop and an MCP coordinator write the same queue. A coordinator
/// that read the queue before a desktop mutation is describing a queue that no
/// longer exists, and every mutator — not just `edit` — has to reject it. The
/// reorder case is the one that used to be undetectable even with a version
/// supplied, because reordering did not move any.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn desktop_writes_invalidate_a_coordinators_stale_queue_mutations() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    for text in ["alpha", "beta", "gamma"] {
        host.session_queue_add(session.id, text.into(), false)
            .unwrap();
    }
    // What the coordinator read before the desktop touched anything.
    let seen = host.session_queue_list(session.id).unwrap();
    let (alpha, beta, gamma) = (seen[0].clone(), seen[1].clone(), seen[2].clone());

    // The desktop reorders underneath it: "gamma" to the head.
    host.session_queue_move(
        session.id,
        &gamma.id,
        0,
        gamma.version,
        host.session_queue_snapshot(session.id).unwrap().revision,
    )
    .unwrap();
    assert_eq!(
        host.session_queue_list(session.id)
            .unwrap()
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha", "beta"]
    );

    // Each of the four mutators, driven from the coordinator's stale view.
    let reorder = orch
        .reorder_queue(
            &auth,
            "stale-reorder",
            session.id,
            ws.path(),
            &beta.id,
            0,
            beta.version,
            host.session_queue_snapshot(session.id).unwrap().revision,
        )
        .await
        .unwrap_err();
    // The revision fence is satisfied; the per-entry CAS is what rejects this.
    assert_eq!(reorder.code.as_str(), "stale_version");

    let remove = orch
        .remove_queue(
            &auth,
            "stale-remove",
            session.id,
            ws.path(),
            &alpha.id,
            alpha.version,
        )
        .await
        .unwrap_err();
    assert_eq!(remove.code.as_str(), "stale_version");

    let run_next = orch
        .run_next_queue(
            &auth,
            "stale-run-next",
            session.id,
            ws.path(),
            &beta.id,
            beta.version,
        )
        .await
        .unwrap_err();
    assert_eq!(run_next.code.as_str(), "stale_version");

    let steer = orch
        .steer_queued(
            &auth,
            "stale-steer-queued",
            session.id,
            ws.path(),
            &alpha.id,
            alpha.version,
        )
        .await
        .unwrap_err();
    assert_eq!(steer.code.as_str(), "stale_version");

    // Nothing the coordinator attempted may have landed.
    assert_eq!(
        host.session_queue_list(session.id)
            .unwrap()
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha", "beta"],
        "no rejected mutation may have been applied"
    );

    // Refetching the versions the desktop left behind lets the coordinator
    // proceed, so this is a conflict to retry, not a permanent wedge.
    let fresh = host.session_queue_list(session.id).unwrap();
    let beta_now = fresh.iter().find(|entry| entry.id == beta.id).unwrap();
    orch.remove_queue(
        &auth,
        "fresh-remove",
        session.id,
        ws.path(),
        &beta_now.id,
        beta_now.version,
    )
    .await
    .unwrap();
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), 2);
    set_grokptah_home_override(None);
}

/// S7: `reject_control_prompt` stops the control plane from *authoring* `!`
/// and `/` prompts, but selection verbs took an entry id and never looked at
/// what they selected. A locally authored admin command could therefore be
/// promoted to the head of the queue by a correctly authenticated coordinator,
/// and `run_next` would cancel the active turn to make it run. Selection must
/// be held to the same policy as authorship.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_coordinator_cannot_schedule_a_locally_authored_command_entry() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    // The desktop is allowed to author these; the control plane is not.
    let slash = host
        .session_queue_add(session.id, "/yolo".into(), false)
        .unwrap()[0]
        .clone();
    let bang = host
        .session_queue_add(session.id, "!rm -rf /tmp/x".into(), false)
        .unwrap()[1]
        .clone();
    let ordinary = host
        .session_queue_add(session.id, "summarise the diff".into(), false)
        .unwrap()[2]
        .clone();

    for (entry, label) in [(&slash, "slash"), (&bang, "bang")] {
        let promoted = orch
            .run_next_queue(
                &auth,
                &format!("run-next-{label}"),
                session.id,
                ws.path(),
                &entry.id,
                entry.version,
            )
            .await
            .unwrap_err();
        assert_eq!(
            promoted.code.as_str(),
            "forbidden_scope",
            "run_next must refuse to schedule a {label} command entry"
        );

        let moved = orch
            .reorder_queue(
                &auth,
                &format!("reorder-{label}"),
                session.id,
                ws.path(),
                &entry.id,
                0,
                entry.version,
                host.session_queue_snapshot(session.id).unwrap().revision,
            )
            .await
            .unwrap_err();
        assert_eq!(
            moved.code.as_str(),
            "forbidden_scope",
            "reorder must refuse to promote a {label} command entry"
        );

        let steered = orch
            .steer_queued(
                &auth,
                &format!("steer-{label}"),
                session.id,
                ws.path(),
                &entry.id,
                entry.version,
            )
            .await
            .unwrap_err();
        assert_eq!(
            steered.code.as_str(),
            "forbidden_scope",
            "steer_queued must refuse to schedule a {label} command entry"
        );
    }

    // Nothing moved, and nothing was cancelled on the strength of a refusal.
    assert_eq!(
        host.session_queue_list(session.id)
            .unwrap()
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
        vec!["/yolo", "!rm -rf /tmp/x", "summarise the diff"],
    );

    // An ordinary entry is still selectable, so this is a policy gate and not
    // a blanket refusal of the verbs.
    orch.run_next_queue(
        &auth,
        "run-next-ordinary",
        session.id,
        ws.path(),
        &ordinary.id,
        ordinary.version,
    )
    .await
    .unwrap();
    assert_eq!(
        host.session_queue_list(session.id).unwrap()[0].text,
        "summarise the diff",
    );
    set_grokptah_home_override(None);
}

/// Selection policy must authorize before reading the queue, or a coordinator
/// could tell a forbidden command exists in another workspace from
/// `forbidden_scope` vs `workspace_mismatch`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn selecting_a_command_in_another_workspace_is_not_an_oracle() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let other = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let slash = host
        .session_queue_add(session.id, "/yolo".into(), false)
        .unwrap()[0]
        .clone();
    let orch = OrchestrationService::new(
        host,
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([
                ws.path().to_path_buf(),
                other.path().to_path_buf(),
            ]),
            max_concurrent_runs: 4,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let run_next = orch
        .run_next_queue(
            &auth,
            "run-next-cross",
            session.id,
            other.path(),
            &slash.id,
            slash.version,
        )
        .await
        .unwrap_err();
    let reorder = orch
        .reorder_queue(
            &auth,
            "reorder-cross",
            session.id,
            other.path(),
            &slash.id,
            0,
            slash.version,
            0,
        )
        .await
        .unwrap_err();
    let steered = orch
        .steer_queued(
            &auth,
            "steer-cross",
            session.id,
            other.path(),
            &slash.id,
            slash.version,
        )
        .await
        .unwrap_err();
    for (label, error) in [
        ("run_next", run_next),
        ("reorder", reorder),
        ("steer_queued", steered),
    ] {
        assert_eq!(
            error.code.as_str(),
            "workspace_mismatch",
            "{label} must fail on the session gate, not leak a command-policy error"
        );
    }
    set_grokptah_home_override(None);
}

/// The desktop is the other writer on this queue. Fencing only the control
/// plane leaves the same absolute-reorder hole open from the desktop side: a
/// coordinator `run_next` displaces entries without changing their versions,
/// so the per-entry CAS alone cannot tell a desktop reorder that its ordering
/// is gone.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn a_coordinator_run_next_invalidates_a_stale_desktop_reorder() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    for text in ["alpha", "beta", "gamma"] {
        host.session_queue_add(session.id, text.into(), false)
            .unwrap();
    }
    // What the desktop is rendering, and the revision it goes with.
    let seen = host.session_queue_snapshot(session.id).unwrap();
    let (alpha, beta, gamma) = (
        seen.entries[0].clone(),
        seen.entries[1].clone(),
        seen.entries[2].clone(),
    );

    // A coordinator promotes gamma. alpha and beta shift; their versions do not.
    orch.run_next_queue(
        &auth,
        "coordinator-run-next",
        session.id,
        ws.path(),
        &gamma.id,
        gamma.version,
    )
    .await
    .unwrap();
    let after = host.session_queue_snapshot(session.id).unwrap();
    assert_eq!(
        after.entries[1].version, alpha.version,
        "run_next must not change displaced entry versions"
    );
    assert!(after.revision > seen.revision);

    // The desktop drag was computed against the old ordering.
    let stale = host
        .session_queue_move(session.id, &beta.id, 0, beta.version, seen.revision)
        .expect_err("a desktop reorder against a superseded ordering must fail");
    assert!(
        stale.to_string().contains("stale prompt queue revision"),
        "unexpected error: {stale}"
    );
    assert_eq!(
        host.session_queue_snapshot(session.id)
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha", "beta"],
        "the losing desktop reorder must not have applied"
    );

    // Re-reading unblocks it, so this is a conflict to retry, not a wedge.
    let fresh = host.session_queue_snapshot(session.id).unwrap();
    let beta_now = fresh
        .entries
        .iter()
        .find(|entry| entry.id == beta.id)
        .unwrap();
    let (reordered, revision) = host
        .session_queue_move(
            session.id,
            &beta_now.id,
            0,
            beta_now.version,
            fresh.revision,
        )
        .unwrap();
    assert_eq!(reordered[0].id, beta.id);
    assert!(revision > fresh.revision);
    set_grokptah_home_override(None);
}

/// Reorder is fenced on the revision, so a coordinator that could not learn
/// the revision its own mutation produced would have to re-read before every
/// reorder — and that read can observe someone else's newer mutation. Every
/// mutation receipt reports the revision it stamped.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn every_queue_mutation_receipt_reports_its_revision() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let mut revisions = Vec::new();
    let queued = orch
        .queue_prompt(&auth, "r1", session.id, ws.path(), "first".into(), false)
        .await
        .unwrap();
    revisions.push(queued["revision"].as_u64().expect("queue_prompt revision"));
    let entry_id = queued["entry"]["id"].as_str().unwrap().to_string();
    let version = queued["entry"]["version"].as_u64().unwrap();

    let edited = orch
        .edit_queue(
            &auth,
            "r2",
            session.id,
            ws.path(),
            &entry_id,
            version,
            "first edited".into(),
        )
        .await
        .unwrap();
    revisions.push(edited["revision"].as_u64().expect("edit revision"));

    let promoted = orch
        .run_next_queue(
            &auth,
            "r3",
            session.id,
            ws.path(),
            &entry_id,
            edited["entry"]["version"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    let run_next_revision = promoted["revision"].as_u64().expect("run_next revision");
    revisions.push(run_next_revision);

    // The point of the field: chain straight into a fenced reorder using what
    // run_next just returned, with no intervening read.
    orch.queue_prompt(&auth, "r4", session.id, ws.path(), "second".into(), false)
        .await
        .unwrap();
    let listed = orch.get_queue(&auth, session.id, ws.path()).unwrap();
    let reordered = orch
        .reorder_queue(
            &auth,
            "r5",
            session.id,
            ws.path(),
            &entry_id,
            1,
            promoted["entry"]["version"].as_u64().unwrap(),
            listed["revision"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    revisions.push(reordered["revision"].as_u64().expect("reorder revision"));

    let removed = orch
        .remove_queue(
            &auth,
            "r6",
            session.id,
            ws.path(),
            &entry_id,
            reordered["entry"]["version"].as_u64().unwrap(),
        )
        .await
        .unwrap();
    revisions.push(removed["revision"].as_u64().expect("remove revision"));

    let cleared = orch
        .clear_queue(&auth, "r7", session.id, ws.path())
        .await
        .unwrap();
    revisions.push(cleared["revision"].as_u64().expect("clear revision"));

    // A revision is only useful if it names this mutation and not an older one.
    assert!(
        revisions.windows(2).all(|pair| pair[1] > pair[0]),
        "receipt revisions must be strictly increasing: {revisions:?}"
    );
    assert_eq!(
        revisions.last().copied(),
        Some(host.session_queue_snapshot(session.id).unwrap().revision),
        "the last receipt must name the queue's current revision"
    );
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn workspace_mismatch_fail_closed() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let other = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host,
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let listed = orch.list_sessions(&auth).unwrap();
    assert_eq!(listed["sessions"][0]["workspaceStatus"], "ready");
    let err = orch
        .queue_prompt(&auth, "r", session.id, other.path(), "x".into(), false)
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "workspace_mismatch");
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn missing_session_workspace_is_not_controllable() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let claimed = ws.path().to_path_buf();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, &claimed).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([claimed.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    drop(ws);

    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let err = orch
        .queue_prompt(&auth, "missing-ws", session.id, &claimed, "x".into(), false)
        .await
        .unwrap_err();
    assert_eq!(err.code.as_str(), "workspace_mismatch");
    assert!(host.session_queue_list(session.id).unwrap().is_empty());
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reject_shell_and_admin_prompts() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host,
        EventBus::new(64),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    assert!(orch
        .queue_prompt(&auth, "a", session.id, ws.path(), "!rm -rf /".into(), false)
        .await
        .is_err());
    assert!(orch
        .queue_prompt(&auth, "b", session.id, ws.path(), "/mcp list".into(), false)
        .await
        .is_err());
    assert!(orch
        .queue_prompt(&auth, "c", session.id, ws.path(), "/yolo".into(), false)
        .await
        .is_err());
    // Validation happens before idempotency is claimed: a rejected payload
    // cannot poison the request ID for a later valid request.
    orch.queue_prompt(
        &auth,
        "c",
        session.id,
        ws.path(),
        "valid follow-up".into(),
        false,
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[test]
fn restart_interrupted_no_auto_resume() {
    let d = tempdir().unwrap();
    let store = OrchStore::open(d.path()).unwrap();
    use chrono::Utc;
    use grokptah_agent_bridge::orchestration::RunRecord;
    let run = RunRecord {
        run_id: "run-x".into(),
        session_id: Uuid::new_v4(),
        workspace: "/w".into(),
        request_id: "q".into(),
        client_id: None,
        state: RunState::Running,
        purpose: Default::default(),
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: RunBounds::default(),
        prompt_preview: "p".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        stop_detail: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    };
    store.save_run(&run).unwrap();
    drop(store);
    let store2 = OrchStore::open(d.path()).unwrap();
    let loaded = store2.load_run("run-x").unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Interrupted);
}

#[test]
fn restart_clears_queued_admission_position() {
    let d = tempdir().unwrap();
    let store = OrchStore::open(d.path()).unwrap();
    let run = grokptah_agent_bridge::orchestration::RunRecord {
        run_id: "queued-restart".into(),
        session_id: Uuid::new_v4(),
        workspace: "/w".into(),
        request_id: "q-restart".into(),
        client_id: Some("mcp".into()),
        state: RunState::Queued,
        purpose: Default::default(),
        agent_id: None,
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: None,
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: Some(3),
        bounds: RunBounds::default(),
        prompt_preview: "p".into(),
        start_seq: None,
        end_seq: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        stop_detail: None,
        aggregates: Default::default(),
        progress: None,
        execution: None,
        approval: None,
    };
    store.save_run(&run).unwrap();
    drop(store);
    let reopened = OrchStore::open(d.path()).unwrap();
    let loaded = reopened.load_run("queued-restart").unwrap().unwrap();
    assert_eq!(loaded.state, RunState::Interrupted);
    assert_eq!(loaded.queue_position, None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn interrupted_run_retry_is_explicit_linked_and_idempotent() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let source_id = "interrupted-source";
    orch.store()
        .save_run(&RunRecord {
            run_id: source_id.into(),
            session_id: session.id,
            workspace: dunce::canonicalize(ws.path())
                .unwrap()
                .display()
                .to_string(),
            request_id: "source-request".into(),
            client_id: Some("mcp".into()),
            state: RunState::Interrupted,
            purpose: Default::default(),
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds {
                max_prompt_bytes: 10_000,
                max_rounds: 2,
                max_duration_ms: 30_000,
                max_total_tokens: None,
            },
            prompt_preview: "previous attempt".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            terminal_result: Some("interrupted".into()),
            final_response: None,
            error_code: Some("interrupted".into()),
            stop_cause: None,
            stop_detail: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();

    let first = orch
        .retry_run(
            &auth,
            "retry-request",
            session.id,
            ws.path(),
            source_id,
            "list files after restart".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(first["sourceRunId"], source_id);
    assert_eq!(first["retryOf"], source_id);
    let retry_id = first["runId"].as_str().unwrap().to_string();
    assert_eq!(
        orch.get_run(&auth, &retry_id).unwrap()["retryOf"],
        source_id
    );
    assert_eq!(
        wait_run_terminal(&orch, &auth, &retry_id, Duration::from_secs(10)).await,
        RunState::Completed
    );

    let widened_bounds = orch
        .retry_run(
            &auth,
            "retry-widened-bounds",
            session.id,
            ws.path(),
            source_id,
            "retry must remain bounded".into(),
            Some(json!({"maxRounds": 3})),
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(widened_bounds.code.as_str(), "invalid_request");

    let replay = orch
        .retry_run(
            &auth,
            "retry-request",
            session.id,
            ws.path(),
            source_id,
            "list files after restart".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap();
    assert_eq!(replay, first);
    let conflict = orch
        .retry_run(
            &auth,
            "retry-request",
            session.id,
            ws.path(),
            source_id,
            "different replacement".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(conflict.code.as_str(), "conflict");

    let other = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(other.id, ws.path()).unwrap();
    let cross_session = orch
        .retry_run(
            &auth,
            "retry-cross-session",
            other.id,
            ws.path(),
            source_id,
            "must not cross ownership".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(cross_session.code.as_str(), "forbidden_scope");

    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // ProcessEnvGuard must span the whole test
async fn e2e_mcp_client_valid_and_invalid_token() {
    let (home, _lock) = setup_home();
    let ws = tempdir().unwrap();
    let host = started_host();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let bus = host.event_bus();
    let _gui = bus.subscribe();
    let orch = OrchestrationService::new(
        host.clone(),
        bus,
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "secret-196".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let srv = start_control_server(orch.clone(), 0).await.unwrap();
    let url = format!("http://{}/mcp", srv.addr);
    let client = reqwest::Client::new();

    let unauth = client
        .post(&url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ptah_list_sessions","arguments":{}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(unauth.status(), 401);

    let list = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"ptah_list_sessions","arguments":{}}}))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 200);

    // benign mutation: queue prompt
    let q = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({
            "jsonrpc":"2.0","id":3,"method":"tools/call",
            "params":{"name":"ptah_queue_prompt","arguments":{
                "request_id":"e2e-q1",
                "session_id": session.id.to_string(),
                "workspace": ws.path().display().to_string(),
                "prompt": "please summarize later"
            }}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        q.status(),
        200,
        "body={}",
        q.text().await.unwrap_or_default()
    );

    // workspace mismatch does not mutate
    let before = host.session_queue_list(session.id).unwrap().len();
    let bad_ws = client
        .post(&url)
        .header("Authorization", "Bearer secret-196")
        .json(&json!({
            "jsonrpc":"2.0","id":4,"method":"tools/call",
            "params":{"name":"ptah_queue_prompt","arguments":{
                "request_id":"e2e-q2",
                "session_id": session.id.to_string(),
                "workspace": "/tmp/not-allowlisted-196",
                "prompt": "nope"
            }}
        }))
        .send()
        .await
        .unwrap();
    assert!(bad_ws.status().is_client_error() || bad_ws.status().is_server_error());
    assert_eq!(host.session_queue_list(session.id).unwrap().len(), before);

    srv.stop();
    set_grokptah_home_override(None);
    let _ = Duration::from_millis(1);
    let _ = PathBuf::from(".");
    let _ = hash_payload(&json!({}));
}

fn orch_for(
    host: &grokptah_agent_bridge::AgentHostHandle,
    home: &tempfile::TempDir,
    ws: &tempfile::TempDir,
    max_concurrent: usize,
) -> std::sync::Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: max_concurrent,
            bounds: RunBounds::default(),
        },
    )
}

async fn wait_run_terminal(
    orch: &OrchestrationService,
    auth: &grokptah_agent_bridge::orchestration::AuthContext,
    run_id: &str,
    timeout: Duration,
) -> RunState {
    let start = std::time::Instant::now();
    loop {
        let v = orch.get_run(auth, run_id).unwrap();
        let state: RunState = serde_json::from_value(v["state"].clone()).unwrap();
        if !matches!(state, RunState::Running | RunState::Queued) {
            return state;
        }
        if start.elapsed() > timeout {
            panic!("run {run_id} still {state:?} after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_run_state(
    orch: &OrchestrationService,
    auth: &grokptah_agent_bridge::orchestration::AuthContext,
    run_id: &str,
    expected: RunState,
    timeout: Duration,
) {
    let start = std::time::Instant::now();
    loop {
        let value = orch.get_run(auth, run_id).unwrap();
        let state: RunState = serde_json::from_value(value["state"].clone()).unwrap();
        if state == expected {
            return;
        }
        assert!(
            !state.is_terminal(),
            "run {run_id} reached {state:?} before {expected:?}"
        );
        if start.elapsed() > timeout {
            panic!("run {run_id} still {state:?} after {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_task_reaches_terminal_offline() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let resp = orch
        .submit_task(
            &auth,
            "sub-1",
            session.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::Completed);
    let run = orch.get_run(&auth, &run_id).unwrap();
    let agent_id = run["agentId"].as_str().unwrap().to_string();
    assert!(!agent_id.is_empty());
    assert_eq!(
        run["bounds"]["maxTotalTokens"],
        grokptah_agent_bridge::DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS
    );
    let handoff = orch.get_handoff(&auth, &run_id).unwrap();
    assert!(handoff["finalResponse"].as_str().is_some());

    // Service-owned Build Runs must publish the same durable checkpoint that
    // the explicit public continuation API consumes. Checkpoint persistence
    // happens after Run finalization, so tolerate that short asynchronous
    // window while keeping the assertion entirely on the public service
    // projection.
    let checkpoint_plan = {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match orch.get_persistent_agent_scoped(&auth, session.id, ws.path(), &agent_id) {
                Ok(plan) if plan["checkpoint"]["checkpointId"].as_str().is_some() => {
                    break plan;
                }
                Ok(_) | Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Ok(_) | Err(_) => panic!("service Run never published a persistent checkpoint"),
            }
        }
    };
    let checkpoint_id = checkpoint_plan["checkpoint"]["checkpointId"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(checkpoint_plan["checkpoint"]["runId"], run_id);
    assert_eq!(
        checkpoint_plan["agent"]["latestCheckpointId"],
        checkpoint_id
    );

    let resumed = orch
        .resume_persistent_agent(
            &auth,
            "continuation-service-test",
            session.id,
            ws.path(),
            &agent_id,
            "Continue with one bounded acknowledgement.".into(),
            Some(1),
        )
        .await
        .unwrap();
    let replayed = orch
        .resume_persistent_agent(
            &auth,
            "continuation-service-test",
            session.id,
            ws.path(),
            &agent_id,
            "Continue with one bounded acknowledgement.".into(),
            Some(1),
        )
        .await
        .unwrap();
    assert_eq!(replayed["response"], resumed["response"]);
    let runs = orch.list_runs_scoped(&auth, session.id, ws.path()).unwrap();
    let resumed_runs = runs["runs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|candidate| candidate["parentRunId"] == run_id)
        .collect::<Vec<_>>();
    assert_eq!(resumed_runs.len(), 1);
    assert_eq!(resumed_runs[0]["state"], "completed");
    assert_eq!(resumed_runs[0]["checkpointId"], checkpoint_id);
    assert!(resumed_runs[0]["continuationContextId"].as_str().is_some());
    // Idempotent retry
    let again = orch
        .submit_task(
            &auth,
            "sub-1",
            session.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    assert_eq!(again["runId"], run_id);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_duration_limit_reached() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let resp = orch
        .submit_task(
            &auth,
            "lim-dur",
            session.id,
            ws.path(),
            "run sleep 5".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 80})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(15)).await;
    assert_eq!(state, RunState::LimitReached);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_session_busy_and_capacity() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s1 = host.session_new_kind(SessionKind::Build).unwrap();
    let s2 = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(s1.id, ws.path()).unwrap();
    host.session_set_cwd(s2.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 1);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let _r1 = orch
        .submit_task(
            &auth,
            "cap-1",
            s1.id,
            ws.path(),
            "run sleep 2".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    // Give the first turn a moment to mark session busy / reserve capacity.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let busy = orch
        .submit_task(
            &auth,
            "cap-busy",
            s1.id,
            ws.path(),
            "list files".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(busy.code.as_str(), "session_busy");

    let cap = orch
        .submit_task(&auth, "cap-2", s2.id, ws.path(), "list files".into(), None)
        .await
        .unwrap_err();
    assert_eq!(cap.code.as_str(), "capacity_exhausted");

    // Atomic capacity: concurrent second reserves must not oversubscribe max=1.
    let cap_snap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap_snap["maxConcurrentRuns"], 1);
    assert!(cap_snap["activeRuns"].as_u64().unwrap() >= 1);

    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn cancel_isolates_sessions() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let a = host.session_new_kind(SessionKind::Build).unwrap();
    let b = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(a.id, ws.path()).unwrap();
    host.session_set_cwd(b.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let ra = orch
        .submit_task(
            &auth,
            "can-a",
            a.id,
            ws.path(),
            "run sleep 8".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 60000})),
        )
        .await
        .unwrap();
    let rb = orch
        .submit_task(
            &auth,
            "can-b",
            b.id,
            ws.path(),
            "list files please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let run_a = ra["runId"].as_str().unwrap().to_string();
    orch.cancel(&auth, "can-req", a.id, ws.path(), Some(&run_a))
        .await
        .unwrap();
    let state_a = wait_run_terminal(&orch, &auth, &run_a, Duration::from_secs(10)).await;
    assert!(
        matches!(
            state_a,
            RunState::Cancelled | RunState::Completed | RunState::Failed
        ),
        "got {state_a:?}"
    );
    // Session B still finishes independently.
    let run_b = rb["runId"].as_str().unwrap().to_string();
    let state_b = wait_run_terminal(&orch, &auth, &run_b, Duration::from_secs(10)).await;
    assert_eq!(state_b, RunState::Completed);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn steer_via_orchestration_service() {
    let (_home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &_home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    // Idle session → steer defers to queue (non-cancelling).
    let idle = orch
        .steer(
            &auth,
            "steer-idle",
            session.id,
            ws.path(),
            "please prefer tests".into(),
        )
        .await
        .unwrap();
    assert_eq!(idle["disposition"], "queued");

    let _run = orch
        .submit_task(
            &auth,
            "steer-run",
            session.id,
            ws.path(),
            "run sleep 3".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let pending = orch
        .steer(
            &auth,
            "steer-live",
            session.id,
            ws.path(),
            "keep going carefully".into(),
        )
        .await
        .unwrap();
    assert_eq!(pending["disposition"], "pending");
    set_grokptah_home_override(None);
}

#[test]
fn queue_survives_host_restart() {
    let (_home, _lock) = setup_home();
    let ws = tempdir().unwrap();
    let session_id = {
        let host = started_host();
        host.set_project_cwd(ws.path()).unwrap();
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        host.session_queue_add(session.id, "follow-up after restart".into(), false)
            .unwrap();
        host.session_queue_add(session.id, "second item".into(), true)
            .unwrap();
        let listed = host.session_queue_list(session.id).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].text, "second item"); // priority front
        session.id
    };
    // New host process-equivalent: same home, fresh AgentHost.
    let host2 = started_host();
    let listed = host2.session_queue_list(session_id).unwrap();
    assert_eq!(listed.len(), 2, "queue must reload from disk");
    assert_eq!(listed[0].text, "second item");
    assert_eq!(listed[1].text, "follow-up after restart");
    set_grokptah_home_override(None);
}

#[test]
fn journal_reload_supports_run_scoped_reads() {
    let dir = tempdir().unwrap();
    let sid = Uuid::new_v4();
    let bus1 = EventBus::new(64).with_persist_dir(dir.path());
    bus1.publish(SessionUpdate::FileEdit {
        session_id: sid,
        path: "a.rs".into(),
        summary: "edited".into(),
        unified_diff: "diff".into(),
    });
    let start = bus1.current_seq();
    drop(bus1);
    let bus2 = EventBus::new(64).with_persist_dir(dir.path());
    let page = bus2.read_after(0, 50);
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].seq, start);
}

#[test]
fn run_event_pages_filter_before_limit_across_sessions() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let other_session = Uuid::new_v4();
    let bus = host.event_bus();
    bus.publish(SessionUpdate::AgentProgress {
        session_id: other_session,
        round: 1,
        max_rounds: 4,
        last_tool: None,
        detail: "unrelated before target".into(),
    });
    bus.publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 1,
        max_rounds: 4,
        last_tool: None,
        detail: "target one".into(),
    });
    let start_seq = bus.current_seq();
    bus.publish(SessionUpdate::AgentProgress {
        session_id: other_session,
        round: 2,
        max_rounds: 4,
        last_tool: None,
        detail: "unrelated between target events".into(),
    });
    bus.publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 2,
        max_rounds: 4,
        last_tool: None,
        detail: "target two".into(),
    });
    let end_seq = bus.current_seq();

    let run_id = Uuid::new_v4().to_string();
    orch.store()
        .save_run(&RunRecord {
            run_id: run_id.clone(),
            session_id: session.id,
            workspace: dunce::canonicalize(ws.path())
                .unwrap()
                .display()
                .to_string(),
            request_id: "event-page-test".into(),
            client_id: None,
            state: RunState::Completed,
            purpose: Default::default(),
            agent_id: None,
            retry_of: None,
            parent_run_id: None,
            agent_spec_revision: None,
            checkpoint_id: None,
            continuation_context_id: None,
            continuation_context_hash: None,
            continuation_fidelity: None,
            queue_position: None,
            bounds: RunBounds::default(),
            prompt_preview: "event page test".into(),
            start_seq: Some(start_seq - 1),
            end_seq: Some(end_seq),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: Some("completed".into()),
            final_response: None,
            error_code: None,
            stop_cause: None,
            stop_detail: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        })
        .unwrap();

    let page = orch.get_events(&auth, Some(&run_id), 0, 1).unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["entries"][0]["seq"], start_seq);
    assert_eq!(page["nextCursor"], start_seq);
    let next = orch
        .get_events(
            &auth,
            Some(&run_id),
            page["nextCursor"].as_u64().unwrap(),
            1,
        )
        .unwrap();
    assert_eq!(next["entries"].as_array().unwrap().len(), 1);
    assert_eq!(next["entries"][0]["seq"], end_seq);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn capacity_race_against_real_submit_task() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    // max_concurrent_runs=2; flood 8 distinct sessions so capacity (not busy) is the gate.
    let mut sessions = Vec::new();
    for _ in 0..8 {
        let s = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(s.id, ws.path()).unwrap();
        sessions.push(s.id);
    }
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let mut futs = Vec::new();
    for (i, sid) in sessions.into_iter().enumerate() {
        let orch = orch.clone();
        let auth = auth.clone();
        let ws_path = ws.path().to_path_buf();
        futs.push(async move {
            orch.submit_task(
                &auth,
                &format!("race-{i}"),
                sid,
                &ws_path,
                "run sleep 3".into(),
                Some(json!({"maxPromptBytes": 10000, "maxRounds": 24, "maxDurationMs": 30000})),
            )
            .await
        });
    }
    let results = futures::future::join_all(futs).await;
    let accepted = results.iter().filter(|r| r.is_ok()).count();
    let exhausted = results
        .iter()
        .filter(|r| {
            r.as_ref()
                .err()
                .map(|e| e.code.as_str() == "capacity_exhausted")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        accepted, 2,
        "exactly max_concurrent_runs must accept under race"
    );
    assert_eq!(exhausted, 6, "remainder must fail capacity_exhausted");
    let cap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap["activeRuns"].as_u64().unwrap(), 2);
    assert_eq!(cap["maxConcurrentRuns"].as_u64().unwrap(), 2);
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn queued_admission_is_bounded_fair_and_cancellable() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let s1 = host.session_new_kind(SessionKind::Build).unwrap();
    let s2 = host.session_new_kind(SessionKind::Build).unwrap();
    let s3 = host.session_new_kind(SessionKind::Build).unwrap();
    let s4 = host.session_new_kind(SessionKind::Build).unwrap();
    for session in [&s1, &s2, &s3, &s4] {
        host.session_set_cwd(session.id, ws.path()).unwrap();
    }
    let orch = orch_for(&host, &home, &ws, 1);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let bounds = Some(json!({
        "maxPromptBytes": 10000,
        "maxRounds": 24,
        "maxDurationMs": 30000
    }));

    let first = orch
        .submit_task(
            &auth,
            "fair-1",
            s1.id,
            ws.path(),
            "run sleep 2".into(),
            bounds.clone(),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let queued_a = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "fair-2a",
            s2.id,
            ws.path(),
            "run sleep 1".into(),
            bounds.clone(),
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_b = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "fair-3",
            s3.id,
            ws.path(),
            "run sleep 1".into(),
            bounds.clone(),
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_same_session = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "fair-2b",
            s2.id,
            ws.path(),
            "list files".into(),
            bounds,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let queued_cancel = orch
        .submit_task_with_execution_mode_and_queue(
            &auth,
            "fair-cancel",
            s4.id,
            ws.path(),
            "list files".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    assert_eq!(queued_a["state"], "queued");
    assert_eq!(queued_b["state"], "queued");
    assert_eq!(queued_same_session["state"], "queued");
    assert_eq!(queued_cancel["state"], "queued");
    assert_eq!(queued_a["queuedPosition"], 1);
    assert_eq!(queued_b["queuedPosition"], 2);
    assert_eq!(queued_same_session["queuedPosition"], 3);
    assert_eq!(queued_cancel["queuedPosition"], 4);
    let cap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap["activeRuns"], 1);
    assert_eq!(cap["queuedRuns"], 4);
    assert_eq!(cap["queueLimit"], 32);

    let cancelled_id = queued_cancel["runId"].as_str().unwrap();
    let cancelled = orch
        .cancel(
            &auth,
            "fair-cancel-request",
            s4.id,
            ws.path(),
            Some(cancelled_id),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["wasQueued"], true);
    assert_eq!(cancelled["teardownComplete"], true);
    assert_eq!(
        orch.get_run(&auth, cancelled_id).unwrap()["stopCause"],
        "cancelled"
    );
    let cap = orch.get_capacity(&auth).unwrap();
    assert_eq!(cap["queuedRuns"], 3);
    assert_eq!(
        orch.get_run(&auth, queued_a["runId"].as_str().unwrap())
            .unwrap()["queuePosition"],
        1
    );
    assert_eq!(
        orch.get_run(&auth, queued_b["runId"].as_str().unwrap())
            .unwrap()["queuePosition"],
        2
    );
    assert_eq!(
        orch.get_run(&auth, queued_same_session["runId"].as_str().unwrap())
            .unwrap()["queuePosition"],
        3
    );

    let first_id = first["runId"].as_str().unwrap().to_string();
    let second_id = queued_a["runId"].as_str().unwrap().to_string();
    let third_id = queued_b["runId"].as_str().unwrap().to_string();
    let same_session_id = queued_same_session["runId"].as_str().unwrap().to_string();
    assert_eq!(
        wait_run_terminal(&orch, &auth, &first_id, Duration::from_secs(10)).await,
        RunState::Completed
    );
    assert_eq!(
        wait_run_terminal(&orch, &auth, &second_id, Duration::from_secs(10)).await,
        RunState::Completed
    );

    // Once s2's first task completes, s3 must run before s2's later task.
    let third_state: RunState =
        serde_json::from_value(orch.get_run(&auth, &third_id).unwrap()["state"].clone()).unwrap();
    let same_session_state: RunState =
        serde_json::from_value(orch.get_run(&auth, &same_session_id).unwrap()["state"].clone())
            .unwrap();
    assert_ne!(third_state, RunState::Queued);
    assert_eq!(same_session_state, RunState::Queued);
    assert_eq!(
        orch.get_run(&auth, &same_session_id).unwrap()["queuePosition"],
        1
    );

    assert_eq!(
        wait_run_terminal(&orch, &auth, &third_id, Duration::from_secs(10)).await,
        RunState::Completed
    );
    assert_eq!(
        wait_run_terminal(&orch, &auth, &same_session_id, Duration::from_secs(10)).await,
        RunState::Completed
    );
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn admitted_run_reserves_session_against_desktop_prompt() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "reserve-1",
            session.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    assert!(host
        .session_prompt(session.id, "desktop collision".into())
        .await
        .is_err());
    let run_id = accepted["runId"].as_str().unwrap();
    orch.cancel(&auth, "reserve-cancel", session.id, ws.path(), Some(run_id))
        .await
        .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn concurrent_same_session_submits_accept_exactly_one() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let one = orch.submit_task(
        &auth,
        "same-session-1",
        session.id,
        ws.path(),
        "run sleep 3".into(),
        None,
    );
    let two = orch.submit_task(
        &auth,
        "same-session-2",
        session.id,
        ws.path(),
        "run sleep 3".into(),
        None,
    );
    let (one, two) = tokio::join!(one, two);
    let results = [one, two];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    let rejected = results
        .iter()
        .find_map(|result| result.as_ref().err())
        .expect("one submit must be rejected");
    assert_eq!(rejected.code.as_str(), "session_busy");
    let accepted = results
        .iter()
        .find_map(|result| result.as_ref().ok())
        .expect("one submit must be accepted");
    orch.cancel(
        &auth,
        "same-session-cancel",
        session.id,
        ws.path(),
        accepted["runId"].as_str(),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn capacity_is_shared_across_control_service_instances() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let first = host.session_new_kind(SessionKind::Build).unwrap();
    let second = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(first.id, ws.path()).unwrap();
    host.session_set_cwd(second.id, ws.path()).unwrap();
    let one = orch_for(&host, &home, &ws, 1);
    let two = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        one.store().clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 8,
            bounds: RunBounds::default(),
        },
    );
    let auth_one = one.auth_header(Some("Bearer t")).unwrap();
    let auth_two = two.auth_header(Some("Bearer t")).unwrap();
    assert_eq!(two.get_capacity(&auth_two).unwrap()["maxConcurrentRuns"], 1);
    let accepted = one
        .submit_task(
            &auth_one,
            "global-cap-1",
            first.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    let error = two
        .submit_task(
            &auth_two,
            "global-cap-2",
            second.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code.as_str(), "capacity_exhausted");
    one.cancel(
        &auth_one,
        "global-cap-cancel",
        first.id,
        ws.path(),
        accepted["runId"].as_str(),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn pending_admission_bound_is_shared_across_control_services() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let active = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(active.id, ws.path()).unwrap();
    let one = orch_for(&host, &home, &ws, 1);
    let two = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        one.store().clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 8,
            bounds: RunBounds::default(),
        },
    );
    let auth_one = one.auth_header(Some("Bearer t")).unwrap();
    let auth_two = two.auth_header(Some("Bearer t")).unwrap();
    let active_run = one
        .submit_task(
            &auth_one,
            "global-queue-active",
            active.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();

    let mut queued = Vec::new();
    for index in 0..32 {
        let session = host.session_new_kind(SessionKind::Build).unwrap();
        host.session_set_cwd(session.id, ws.path()).unwrap();
        let (service, auth) = if index % 2 == 0 {
            (&one, &auth_one)
        } else {
            (&two, &auth_two)
        };
        let response = service
            .submit_task_with_execution_mode_and_queue(
                auth,
                &format!("global-queue-{index}"),
                session.id,
                ws.path(),
                "list files".into(),
                None,
                RunExecutionMode::Shared,
                true,
            )
            .await
            .unwrap();
        queued.push((service.clone(), auth.clone(), session.id, response));
    }
    assert_eq!(one.get_capacity(&auth_one).unwrap()["queuedRuns"], 32);
    assert_eq!(two.get_capacity(&auth_two).unwrap()["queuedRuns"], 32);

    let overflow_session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(overflow_session.id, ws.path())
        .unwrap();
    let overflow = one
        .submit_task_with_execution_mode_and_queue(
            &auth_one,
            "global-queue-overflow",
            overflow_session.id,
            ws.path(),
            "list files".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(overflow.code.as_str(), "capacity_exhausted");
    assert_eq!(one.get_capacity(&auth_one).unwrap()["queuedRuns"], 32);

    for (service, auth, session_id, response) in queued {
        service
            .cancel(
                &auth,
                &format!("global-queue-cancel-{session_id}"),
                session_id,
                ws.path(),
                response["runId"].as_str(),
            )
            .await
            .unwrap();
    }
    one.cancel(
        &auth_one,
        "global-queue-cancel-active",
        active.id,
        ws.path(),
        active_run["runId"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(one.get_capacity(&auth_one).unwrap()["queuedRuns"], 0);
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn global_scheduler_fairness_spans_control_services() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();

    let active = host.session_new_kind(SessionKind::Build).unwrap();
    let session_a = host.session_new_kind(SessionKind::Build).unwrap();
    let session_b = host.session_new_kind(SessionKind::Build).unwrap();
    for session in [&active, &session_a, &session_b] {
        host.session_set_cwd(session.id, ws.path()).unwrap();
    }

    let one = orch_for(&host, &home, &ws, 1);
    let two = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        one.store().clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 8,
            bounds: RunBounds::default(),
        },
    );
    let auth_one = one.auth_header(Some("Bearer t")).unwrap();
    let auth_two = two.auth_header(Some("Bearer t")).unwrap();

    let active_run = one
        .submit_task(
            &auth_one,
            "fair-global-active",
            active.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    wait_run_state(
        &one,
        &auth_one,
        active_run["runId"].as_str().unwrap(),
        RunState::Running,
        Duration::from_secs(2),
    )
    .await;
    let first = one
        .submit_task_with_execution_mode_and_queue(
            &auth_one,
            "fair-global-a1",
            session_a.id,
            ws.path(),
            "run sleep 2".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let second = one
        .submit_task_with_execution_mode_and_queue(
            &auth_one,
            "fair-global-a2",
            session_a.id,
            ws.path(),
            "run sleep 2".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    let third = two
        .submit_task_with_execution_mode_and_queue(
            &auth_two,
            "fair-global-b1",
            session_b.id,
            ws.path(),
            "run sleep 2".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();

    assert_eq!(first["queuedPosition"], 1);
    assert_eq!(second["queuedPosition"], 2);
    assert_eq!(third["queuedPosition"], 3);

    one.cancel(
        &auth_one,
        "fair-global-cancel-active",
        active.id,
        ws.path(),
        active_run["runId"].as_str(),
    )
    .await
    .unwrap();
    let first_id = first["runId"].as_str().unwrap();
    let second_id = second["runId"].as_str().unwrap();
    let third_id = third["runId"].as_str().unwrap();
    wait_run_state(
        &one,
        &auth_one,
        first_id,
        RunState::Running,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        serde_json::from_value::<RunState>(
            one.get_run(&auth_one, second_id).unwrap()["state"].clone()
        )
        .unwrap(),
        RunState::Queued,
        "same-service later work must wait behind its session's older run"
    );
    assert_eq!(
        serde_json::from_value::<RunState>(
            two.get_run(&auth_two, third_id).unwrap()["state"].clone()
        )
        .unwrap(),
        RunState::Queued,
        "different service work must remain queued while the selected run is active"
    );
    assert_eq!(
        one.get_progress(&auth_one, second_id).unwrap()["queuePosition"],
        1
    );
    assert_eq!(
        two.get_progress(&auth_two, third_id).unwrap()["queuePosition"],
        2
    );

    wait_run_terminal(&one, &auth_one, first_id, Duration::from_secs(8)).await;
    wait_run_state(
        &two,
        &auth_two,
        third_id,
        RunState::Running,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(
        serde_json::from_value::<RunState>(
            one.get_run(&auth_one, second_id).unwrap()["state"].clone()
        )
        .unwrap(),
        RunState::Queued,
        "the global scheduler must prefer service B after service A starts"
    );
    assert_eq!(
        one.get_progress(&auth_one, second_id).unwrap()["queuePosition"],
        1
    );

    wait_run_terminal(&two, &auth_two, third_id, Duration::from_secs(8)).await;
    wait_run_terminal(&one, &auth_one, second_id, Duration::from_secs(8)).await;
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn dropping_control_service_releases_pending_admission_slot() {
    std::env::set_var("GROKPTAH_AGENT_OFFLINE", "1");
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let active = host.session_new_kind(SessionKind::Build).unwrap();
    let queued = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(active.id, ws.path()).unwrap();
    host.session_set_cwd(queued.id, ws.path()).unwrap();
    let primary = orch_for(&host, &home, &ws, 1);
    let queued_service = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        primary.store().clone(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 8,
            bounds: RunBounds::default(),
        },
    );
    let primary_auth = primary.auth_header(Some("Bearer t")).unwrap();
    let queued_auth = queued_service.auth_header(Some("Bearer t")).unwrap();
    let active_run = primary
        .submit_task(
            &primary_auth,
            "drop-active",
            active.id,
            ws.path(),
            "run sleep 3".into(),
            None,
        )
        .await
        .unwrap();
    queued_service
        .submit_task_with_execution_mode_and_queue(
            &queued_auth,
            "drop-queued",
            queued.id,
            ws.path(),
            "list files".into(),
            None,
            RunExecutionMode::Shared,
            true,
        )
        .await
        .unwrap();
    assert_eq!(
        primary.get_capacity(&primary_auth).unwrap()["queuedRuns"],
        1
    );
    drop(queued_service);
    assert_eq!(
        primary.get_capacity(&primary_auth).unwrap()["queuedRuns"],
        0
    );
    primary
        .cancel(
            &primary_auth,
            "drop-cancel",
            active.id,
            ws.path(),
            active_run["runId"].as_str(),
        )
        .await
        .unwrap();
    set_grokptah_home_override(None);
    std::env::remove_var("GROKPTAH_AGENT_OFFLINE");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn the_progress_projection_is_redacted_and_reports_the_stop_detail() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    let secret_prompt = "SENSITIVE-PROMPT /home/someone/.ssh/id_ed25519 hunter2";
    let accepted = orch
        .submit_task(
            &auth,
            "redaction-1",
            session.id,
            ws.path(),
            secret_prompt.into(),
            None,
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap().to_string();

    let progress = orch.get_progress(&auth, &run_id).unwrap();
    let encoded = serde_json::to_string(&progress).unwrap();

    // The user's own prompt must not ride along on a status read.
    assert!(
        progress.get("promptPreview").is_none(),
        "progress projection still exposes promptPreview"
    );
    for leak in ["SENSITIVE-PROMPT", "/home/someone", "id_ed25519", "hunter2"] {
        assert!(!encoded.contains(leak), "progress projection leaked {leak}");
    }

    // What it does carry is the structured, operator-readable stop.
    // The projection is versioned, so a consumer can tell shape 2 (redacted,
    // with stopDetail) from the historical shape 1 that carried promptPreview.
    assert_eq!(progress["schemaVersion"], 2);
    // Detail-bearing assertions live in the in-crate contract module: a stop
    // detail cannot be minted from outside the crate, which is the point.
    orch.cancel(
        &auth,
        "redaction-cancel",
        session.id,
        ws.path(),
        Some(&run_id),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn agent_progress_is_durable_outside_event_retention() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 2);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "progress-1",
            session.id,
            ws.path(),
            "run sleep 2".into(),
            None,
        )
        .await
        .unwrap();
    host.event_bus().publish(SessionUpdate::AgentProgress {
        session_id: session.id,
        round: 3,
        max_rounds: 8,
        last_tool: Some("shell".into()),
        detail: "verifying".into(),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let run_id = accepted["runId"].as_str().unwrap();
    let progress = orch.get_progress(&auth, run_id).unwrap();
    assert_eq!(progress["progress"]["round"], 3);
    assert_eq!(progress["progress"]["lastTool"], "shell");
    orch.cancel(
        &auth,
        "progress-cancel",
        session.id,
        ws.path(),
        Some(run_id),
    )
    .await
    .unwrap();
    set_grokptah_home_override(None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_round_limit_reached_via_wired_max_rounds() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    // Offline path honors turn_max_rounds for simulate_tool_rounds prompts.
    let resp = orch
        .submit_task(
            &auth,
            "round-lim",
            session.id,
            ws.path(),
            "simulate_tool_rounds please".into(),
            Some(json!({"maxPromptBytes": 10000, "maxRounds": 2, "maxDurationMs": 30000})),
        )
        .await
        .unwrap();
    let run_id = resp["runId"].as_str().unwrap().to_string();
    let state = wait_run_terminal(&orch, &auth, &run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::LimitReached);
    let handoff = orch.get_handoff(&auth, &run_id).unwrap();
    let text = handoff["finalResponse"].as_str().unwrap_or("");
    assert!(
        text.contains("Stopped after 2 tool rounds"),
        "expected stop message reflecting max_rounds=2, got {text:?}"
    );
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn token_ceiling_wins_over_round_limit_after_last_round_tool_calls() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(|| async {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": null,
                        "tool_calls": [{
                            "id": "call-write-at-ceiling",
                            "type": "function",
                            "function": {
                                "name": "write_file",
                                "arguments": "{\"path\":\"ceiling-proof.txt\",\"content\":\"settled\\n\"}"
                            }
                        }]
                    }
                }],
                "usage": {
                    "prompt_tokens": 6,
                    "completion_tokens": 4,
                    "total_tokens": 10
                }
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (home, mut guard) = setup_home();
    guard.remove("GROKPTAH_AGENT_OFFLINE");
    guard.set("GROKPTAH_API_BASE", format!("http://{address}/v1"));
    guard.set("GROKPTAH_API_KEY", "synthetic-compatible-key");
    let host = started_host();
    host.set_model(model_selection_key("env-grokptah", "synthetic-cheap-code"));
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds {
                max_rounds: 1,
                max_total_tokens: Some(10),
                ..RunBounds::default()
            },
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "token-last-round",
            session.id,
            ws.path(),
            "Write the proof file.".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap();
    let state = wait_run_terminal(&orch, &auth, run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::LimitReached);
    let run = orch.get_run(&auth, run_id).unwrap();
    assert_eq!(run["stopCause"], "token_ceiling");
    assert_eq!(run["errorCode"], "max_total_tokens_reached");
    assert_eq!(run["aggregates"]["usage"]["totalTokens"], 10);
    assert_eq!(
        std::fs::read_to_string(ws.path().join("ceiling-proof.txt")).unwrap(),
        "settled\n"
    );

    host.stop().unwrap();
    server.abort();
    set_grokptah_home_override(None);
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::await_holding_lock)]
async fn bounded_compaction_uses_durable_admission_and_stops_before_the_main_model_call() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route(
            "/v1/chat/completions",
            post(|State(requests): State<Arc<AtomicUsize>>| async move {
                requests.fetch_add(1, Ordering::SeqCst);
                Json(json!({
                    "choices": [{"message": {"content": "Compacted durable context."}}],
                    "usage": {
                        "prompt_tokens": 6,
                        "completion_tokens": 4,
                        "total_tokens": 10
                    }
                }))
            }),
        )
        .with_state(requests.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let (home, mut guard) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    for index in 0..21 {
        host.session_prompt(session.id, format!("offline context turn {index}"))
            .await
            .unwrap();
    }

    guard.remove("GROKPTAH_AGENT_OFFLINE");
    guard.set("GROKPTAH_API_BASE", format!("http://{address}/v1"));
    guard.set("GROKPTAH_API_KEY", "synthetic-compatible-key");
    let synthetic_model = model_selection_key("env-grokptah", "synthetic-cheap-code");
    host.set_model(synthetic_model.clone());
    let agent = host.ensure_session_agent(session.id).unwrap();
    host.ensure_orchestration_store()
        .unwrap()
        .revise_agent_spec(&agent.agent_id, "test:bounded-compaction-model", |spec| {
            spec.model = AgentModelSpec::from_selection_key(&synthetic_model)
                .expect("synthetic model selection must be valid");
            Ok(())
        })
        .unwrap()
        .expect("persistent Agent must exist");
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "t".into(),
            allowlist: WorkspaceAllowlist::new([ws.path().to_path_buf()]),
            max_concurrent_runs: 1,
            bounds: RunBounds {
                max_rounds: 3,
                max_total_tokens: Some(10),
                ..RunBounds::default()
            },
        },
    );
    let auth = orch.auth_header(Some("Bearer t")).unwrap();
    let accepted = orch
        .submit_task(
            &auth,
            "bounded-compact",
            session.id,
            ws.path(),
            "Continue after compacting the accumulated context.".into(),
            None,
        )
        .await
        .unwrap();
    let run_id = accepted["runId"].as_str().unwrap();
    let state = wait_run_terminal(&orch, &auth, run_id, Duration::from_secs(10)).await;
    assert_eq!(state, RunState::LimitReached);
    let run = orch.get_run(&auth, run_id).unwrap();
    assert_eq!(run["stopCause"], "token_ceiling");
    assert_eq!(run["aggregates"]["usage"]["totalTokens"], 10);
    assert_eq!(run["aggregates"]["usagePendingRequests"], 0);
    assert_eq!(requests.load(Ordering::SeqCst), 1);

    host.stop().unwrap();
    server.abort();
    set_grokptah_home_override(None);
}

/// The per-entry CAS cannot see a promotion that leaves displaced entries'
/// versions alone. An absolute reorder therefore also needs the queue
/// revision, which changes for the competing `run_next`.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn run_next_invalidates_a_stale_reorder_revision() {
    let (home, _lock) = setup_home();
    let host = started_host();
    let ws = tempdir().unwrap();
    host.set_project_cwd(ws.path()).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, ws.path()).unwrap();
    let orch = orch_for(&host, &home, &ws, 4);
    let auth = orch.auth_header(Some("Bearer t")).unwrap();

    for text in ["alpha", "beta", "gamma"] {
        host.session_queue_add(session.id, text.into(), false)
            .unwrap();
    }
    let seen_snapshot = host.session_queue_snapshot(session.id).unwrap();
    let (seen_revision, seen) = (seen_snapshot.revision, seen_snapshot.entries);
    let (alpha, beta, gamma) = (seen[0].clone(), seen[1].clone(), seen[2].clone());

    host.session_queue_run_next(session.id, &gamma.id, gamma.version)
        .unwrap();
    let current_snapshot = host.session_queue_snapshot(session.id).unwrap();
    let (current_revision, after_run_next) = (current_snapshot.revision, current_snapshot.entries);
    assert!(current_revision > seen_revision);
    assert_eq!(
        after_run_next
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha", "beta"]
    );
    assert_eq!(
        after_run_next
            .iter()
            .find(|entry| entry.id == alpha.id)
            .unwrap()
            .version,
        alpha.version,
        "run_next must not change displaced entry versions"
    );

    let stale = orch
        .reorder_queue(
            &auth,
            "stale-reorder-after-run-next",
            session.id,
            ws.path(),
            &beta.id,
            0,
            beta.version,
            seen_revision,
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code.as_str(), "stale_version");
    assert_eq!(
        host.session_queue_snapshot(session.id)
            .unwrap()
            .entries
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>(),
        vec!["gamma", "alpha", "beta"],
        "a stale absolute reorder must not apply"
    );

    let fresh_snapshot = host.session_queue_snapshot(session.id).unwrap();
    let (fresh_revision, fresh) = (fresh_snapshot.revision, fresh_snapshot.entries);
    let beta_now = fresh.iter().find(|entry| entry.id == beta.id).unwrap();
    let reordered = orch
        .reorder_queue(
            &auth,
            "fresh-reorder-after-run-next",
            session.id,
            ws.path(),
            &beta_now.id,
            0,
            beta_now.version,
            fresh_revision,
        )
        .await
        .unwrap();
    assert_eq!(reordered["revision"], fresh_revision + 1);
    assert_eq!(reordered["entries"][0]["id"], beta.id);
    set_grokptah_home_override(None);
}
