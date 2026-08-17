//! Protocol-level remote-service conformance and soak coverage.
//!
//! These tests speak MCP over HTTP against a disposable `grokptah-service`
//! listener. They do not call a model. Capacity, idempotency, authorization,
//! reconnect, and desktop-facing receipts are proven at the wire.

mod common;

use std::time::Duration;

use grokptah_agent_bridge::{LiveNotification, McpControlClient, RunScope};
use serde_json::json;
use tokio::time::timeout;
use uuid::Uuid;

use common::{
    assert_submission_shape, create_build_session, err_text, flood_journal, mcp_client,
    point_agent_at_new_run, post_mcp_and_drop, post_mcp_partial_and_drop, seed_active_run,
    start_isolated, ServiceEnv, TOKEN,
};

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn multiple_authenticated_clients_share_one_service() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 2).await;
    let mut alice = mcp_client(handle.addr).await;
    let mut bob = mcp_client(handle.addr).await;
    assert_ne!(
        alice.session_id(),
        bob.session_id(),
        "each MCP client must receive its own transport session"
    );

    let session_id = create_build_session(&mut alice, &workspace, "Shared session").await;
    let listed = bob
        .call_tool("ptah_list_sessions", json!({}))
        .await
        .expect("bob list sessions");
    assert!(!listed.is_error, "{:?}", listed.raw);
    assert!(listed.structured["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["sessionId"] == session_id.to_string()));

    let alice_cap = alice
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    let bob_cap = bob.call_tool("ptah_get_capacity", json!({})).await.unwrap();
    assert_eq!(alice_cap.structured["maxConcurrentRuns"], 2);
    assert_eq!(
        alice_cap.structured["available"],
        bob_cap.structured["available"]
    );

    alice.close_session().await.unwrap();
    bob.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn capacity_one_active_is_fail_fast_fifo_and_promotes_after_release() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 1).await;
    let host = handle.host();
    let mut client = mcp_client(handle.addr).await;

    let first = create_build_session(&mut client, &workspace, "Capacity holder").await;
    let second = create_build_session(&mut client, &workspace, "Capacity queue").await;
    host.reserve_orchestration_turn("conformance-hold", first)
        .unwrap();

    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["maxConcurrentRuns"], 1);
    assert_eq!(capacity.structured["activeRuns"], 1);
    assert_eq!(capacity.structured["available"], 0);
    assert_eq!(capacity.structured["queueLimit"], 32);

    let fail_fast = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-fail-fast",
                "session_id": second,
                "workspace": workspace,
                "prompt": "must be rejected while capacity is held",
                "execution_mode": "shared",
                "allow_queue": false,
            }),
        )
        .await;
    let fail_fast = err_text(&fail_fast);
    assert!(
        fail_fast.to_ascii_lowercase().contains("capacity"),
        "fail-fast must be a capacity error, got {fail_fast}"
    );

    let queued_a = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-queued-a",
                "session_id": second,
                "workspace": workspace,
                "prompt": "first queued session",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert!(!queued_a.is_error, "{:?}", queued_a.raw);
    assert_submission_shape(&queued_a.structured);
    assert_eq!(queued_a.structured["state"], "queued");
    assert_eq!(queued_a.structured["queuedPosition"], 1);
    let queued_a_id = queued_a.structured["runId"].as_str().unwrap().to_string();

    let third = create_build_session(&mut client, &workspace, "Second queued session").await;
    let queued_b = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-queued-b",
                "session_id": third,
                "workspace": workspace,
                "prompt": "second queued session",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(queued_b.structured["state"], "queued");
    assert_eq!(queued_b.structured["queuedPosition"], 2);
    let queued_b_id = queued_b.structured["runId"].as_str().unwrap().to_string();

    let queued_a2 = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-queued-a2",
                "session_id": second,
                "workspace": workspace,
                "prompt": "same-session later work",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(queued_a2.structured["queuedPosition"], 3);
    let queued_a2_id = queued_a2.structured["runId"].as_str().unwrap().to_string();

    let cancelled = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conformance-cancel-queued-b",
                "session_id": third,
                "workspace": workspace,
                "run_id": queued_b_id,
            }),
        )
        .await
        .unwrap();
    assert_eq!(cancelled.structured["cancelled"], true);

    let after_cancel = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(after_cancel.structured["queuedRuns"], 2);
    let a_after = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": second,
                "workspace": workspace,
                "run_id": queued_a_id,
            }),
        )
        .await
        .unwrap();
    let a2_after = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": second,
                "workspace": workspace,
                "run_id": queued_a2_id,
            }),
        )
        .await
        .unwrap();
    assert_eq!(a_after.structured["queuePosition"], 1);
    assert_eq!(a2_after.structured["queuePosition"], 2);

    host.release_orchestration_turn("conformance-hold");
    let promoted = wait_run_not_queued(&mut client, second, &workspace, &queued_a_id).await;
    assert_ne!(
        promoted, "queued",
        "capacity release must promote FIFO head"
    );

    let later = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": second,
                "workspace": workspace,
                "run_id": queued_a2_id,
            }),
        )
        .await
        .unwrap();
    if promoted == "running" {
        assert_eq!(
            later.structured["state"], "queued",
            "same-session later work must wait behind the promoted head"
        );
    }

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn bounded_queue_rejects_the_thirty_third_admission() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 1).await;
    let host = handle.host();
    let mut client = mcp_client(handle.addr).await;
    let holder = create_build_session(&mut client, &workspace, "Bound holder").await;
    let queued = create_build_session(&mut client, &workspace, "Bound queue").await;
    host.reserve_orchestration_turn("conformance-bound-hold", holder)
        .unwrap();

    for index in 0..32 {
        let accepted = client
            .call_tool(
                "ptah_submit_task",
                json!({
                    "request_id": format!("conformance-bound-{index}"),
                    "session_id": queued,
                    "workspace": workspace,
                    "prompt": format!("queued admission {index}"),
                    "allow_queue": true,
                }),
            )
            .await
            .unwrap();
        assert_eq!(accepted.structured["state"], "queued");
        assert_eq!(accepted.structured["queuedPosition"], index + 1);
    }
    let overflow = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-bound-overflow",
                "session_id": queued,
                "workspace": workspace,
                "prompt": "one past the bound",
                "allow_queue": true,
            }),
        )
        .await;
    let overflow = err_text(&overflow);
    assert!(
        overflow.to_ascii_lowercase().contains("capacity")
            || overflow.to_ascii_lowercase().contains("full"),
        "33rd admission must fail closed, got {overflow}"
    );
    let capacity = client
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .unwrap();
    assert_eq!(capacity.structured["queuedRuns"], 32);

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn submit_idempotency_replays_receipt_and_rejects_payload_reuse() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 1).await;
    let host = handle.host();
    let mut client = mcp_client(handle.addr).await;
    let holder = create_build_session(&mut client, &workspace, "Idempotency holder").await;
    let session = create_build_session(&mut client, &workspace, "Idempotency session").await;
    host.reserve_orchestration_turn("conformance-idemp-hold", holder)
        .unwrap();

    let args = json!({
        "request_id": "conformance-idemp-1",
        "session_id": session,
        "workspace": workspace,
        "prompt": "identical payload",
        "execution_mode": "shared",
        "allow_queue": true,
    });
    let first = client
        .call_tool("ptah_submit_task", args.clone())
        .await
        .unwrap();
    assert_submission_shape(&first.structured);
    let replay = client.call_tool("ptah_submit_task", args).await.unwrap();
    assert_eq!(replay.structured["runId"], first.structured["runId"]);
    assert_eq!(replay.structured["requestId"], "conformance-idemp-1");
    assert_eq!(replay.structured["state"], first.structured["state"]);

    let conflict = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "conformance-idemp-1",
                "session_id": session,
                "workspace": workspace,
                "prompt": "different payload must fail closed",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await;
    let conflict = err_text(&conflict).to_ascii_lowercase();
    assert!(
        conflict.contains("request_id")
            || conflict.contains("payload")
            || conflict.contains("reused"),
        "reused request id with a different payload must fail closed, got {conflict}"
    );

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn authorization_is_fail_closed_across_token_session_and_workspace() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let other = env.other_workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone(), other.clone()], 2).await;
    let host = handle.host();
    let (victim_session, victim_run, _, _) = seed_active_run(&host, &workspace, "authz-victim");
    let mut client = mcp_client(handle.addr).await;
    let peer = create_build_session(&mut client, &other, "Peer session").await;

    let mut wrong = McpControlClient::new(format!("http://{}", handle.addr), "wrong-token");
    let wrong_init = wrong.initialize().await;
    assert!(
        err_text(&wrong_init).to_ascii_lowercase().contains("401")
            || err_text(&wrong_init)
                .to_ascii_lowercase()
                .contains("unauthor"),
        "wrong bearer must fail closed: {}",
        err_text(&wrong_init)
    );

    let unknown_session = client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": Uuid::new_v4(),
                "workspace": workspace,
            }),
        )
        .await;
    assert!(
        unknown_session.is_err(),
        "unknown session must not list runs: {unknown_session:?}"
    );

    let mismatched = client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": victim_session,
                "workspace": other,
            }),
        )
        .await;
    let mismatched = err_text(&mismatched).to_ascii_lowercase();
    assert!(
        mismatched.contains("workspace") || mismatched.contains("mismatch"),
        "mismatched workspace must fail closed: {mismatched}"
    );

    let outside = tempfile::tempdir().unwrap();
    let not_allowlisted = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": outside.path(),
                "title": "must not exist",
            }),
        )
        .await;
    assert!(
        not_allowlisted.is_err(),
        "non-allowlisted create must fail: {not_allowlisted:?}"
    );

    let cross_read = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": peer,
                "workspace": other,
                "run_id": victim_run,
            }),
        )
        .await;
    let cross_read = err_text(&cross_read).to_ascii_lowercase();
    assert!(
        cross_read.contains("forbidden")
            || cross_read.contains("scope")
            || cross_read.contains("belong"),
        "cross-session read must fail closed: {cross_read}"
    );

    let cross_events = client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": peer,
                "workspace": other,
                "run_id": victim_run,
                "after_seq": 0,
                "limit": 20,
            }),
        )
        .await;
    assert!(
        cross_events.is_err(),
        "cross-session events must fail: {cross_events:?}"
    );

    let cross_cancel = client
        .call_tool(
            "ptah_cancel",
            json!({
                "request_id": "conformance-cross-cancel",
                "session_id": peer,
                "workspace": other,
                "run_id": victim_run,
            }),
        )
        .await;
    let cross_cancel = err_text(&cross_cancel).to_ascii_lowercase();
    assert!(
        cross_cancel.contains("forbidden")
            || cross_cancel.contains("scope")
            || cross_cancel.contains("belong"),
        "cross-session cancel must fail closed: {cross_cancel}"
    );

    let victim_still = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": victim_session,
                "workspace": workspace,
                "run_id": victim_run,
            }),
        )
        .await
        .unwrap();
    assert_eq!(victim_still.structured["state"], "running");

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn disconnect_reconnect_restart_and_cursor_expiry_are_durable() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 1).await;
    let host = handle.host();
    let (session_id, run_id, _, first_seq) = seed_active_run(&host, &workspace, "resilience-run");
    let mut client = mcp_client(handle.addr).await;
    let holder = create_build_session(&mut client, &workspace, "Disconnect holder").await;
    let submit_session = create_build_session(&mut client, &workspace, "Disconnect session").await;
    host.reserve_orchestration_turn("conformance-disc-hold", holder)
        .unwrap();

    let transport = client
        .session_id()
        .expect("initialized transport session")
        .to_string();
    let request_id = "conformance-disconnect-full";
    let body = json!({
        "jsonrpc": "2.0",
        "id": 70,
        "method": "tools/call",
        "params": {
            "name": "ptah_submit_task",
            "arguments": {
                "request_id": request_id,
                "session_id": submit_session,
                "workspace": workspace,
                "prompt": "disconnect once",
                "execution_mode": "shared",
                "allow_queue": true
            }
        }
    });
    post_mcp_and_drop(handle.addr, TOKEN, &transport, &body).unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let replay = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": request_id,
                "session_id": submit_session,
                "workspace": workspace,
                "prompt": "disconnect once",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_submission_shape(&replay.structured);
    assert_eq!(replay.structured["requestId"], request_id);

    let partial_id = "conformance-disconnect-partial";
    let partial_body = json!({
        "jsonrpc": "2.0",
        "id": 71,
        "method": "tools/call",
        "params": {
            "name": "ptah_submit_task",
            "arguments": {
                "request_id": partial_id,
                "session_id": submit_session,
                "workspace": workspace,
                "prompt": "partial body should not commit",
                "allow_queue": true
            }
        }
    });
    post_mcp_partial_and_drop(handle.addr, TOKEN, &transport, &partial_body).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    let after_partial = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": partial_id,
                "session_id": submit_session,
                "workspace": workspace,
                "prompt": "partial body should not commit",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(after_partial.structured["requestId"], partial_id);

    let stale = client.session_id().unwrap().to_string();
    client.close_session().await.unwrap();
    let mut stale_client = McpControlClient::new(format!("http://{}", handle.addr), TOKEN);
    // Force the closed transport session onto a fresh client object.
    let _ = stale_client
        .rpc_raw("2.0", "tools/list", json!({}), true)
        .await;
    client = mcp_client(handle.addr).await;
    assert_ne!(client.session_id(), Some(stale.as_str()));
    let after_reconnect = client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id,
            }),
        )
        .await
        .unwrap();
    assert_eq!(after_reconnect.structured["runId"], run_id);

    let scope = RunScope {
        session_id,
        workspace: workspace.display().to_string(),
        run_id: run_id.clone(),
    };
    let mut stream = client
        .open_event_stream(scope.clone(), Some(first_seq.saturating_sub(1)))
        .await
        .unwrap();
    let frame = timeout(Duration::from_secs(2), stream.next_notification())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match frame.notification {
        LiveNotification::Event(event) => assert_eq!(event.seq, first_seq),
        other => panic!("expected replayed event, got {other:?}"),
    }
    drop(stream);

    flood_journal(&host, 5000);
    let expired = client
        .call_tool(
            "ptah_get_events",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id,
                "after_seq": first_seq,
                "limit": 20,
            }),
        )
        .await;
    let expired = err_text(&expired).to_ascii_lowercase();
    assert!(
        expired.contains("cursor_expired")
            || expired.contains("410")
            || expired.contains("expired"),
        "cursor below retention must fail closed: {expired}"
    );
    let expired_stream = client.open_event_stream(scope, Some(first_seq)).await;
    assert!(
        expired_stream.is_err(),
        "live stream must not silently skip an expired cursor: {expired_stream:?}"
    );

    client.close_session().await.unwrap();
    drop(host);
    handle.stop_and_wait().await;

    let restarted = start_isolated(&env, vec![workspace.clone()], 1).await;
    let mut restarted_client = mcp_client(restarted.addr).await;
    let recovered = restarted_client
        .call_tool(
            "ptah_get_run",
            json!({
                "session_id": session_id,
                "workspace": workspace,
                "run_id": run_id,
            }),
        )
        .await
        .unwrap();
    assert!(!recovered.is_error, "{:?}", recovered.raw);
    assert_eq!(recovered.structured["runId"], run_id);
    restarted_client.close_session().await.unwrap();
    restarted.stop_and_wait().await;
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn desktop_contract_lists_history_after_active_pointer_changes() {
    let env = ServiceEnv::new();
    let workspace = env.workspace_path();
    let handle = start_isolated(&env, vec![workspace.clone()], 2).await;
    let host = handle.host();
    let (session_id, first_run, _, _) = seed_active_run(&host, &workspace, "desktop-history-1");
    point_agent_at_new_run(
        &host,
        session_id,
        &workspace,
        &first_run,
        "desktop-history-2",
    );

    let mut client = mcp_client(handle.addr).await;
    let created = client
        .call_tool(
            "ptah_create_session",
            json!({
                "workspace": workspace,
                "title": "Desktop contract session",
            }),
        )
        .await
        .unwrap();
    assert!(!created.is_error, "{:?}", created.raw);
    assert_eq!(created.structured["title"], "Desktop contract session");
    assert_eq!(
        created.structured["workspace"],
        workspace.display().to_string()
    );
    let listed = client
        .call_tool("ptah_list_sessions", json!({}))
        .await
        .unwrap();
    assert!(listed.structured["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|session| session["sessionId"] == created.structured["sessionId"]));

    host.reserve_orchestration_turn("desktop-contract-hold", session_id)
        .ok();
    let submitted = client
        .call_tool(
            "ptah_submit_task",
            json!({
                "request_id": "desktop-contract-submit",
                "session_id": created.structured["sessionId"],
                "workspace": workspace,
                "prompt": "typed submission receipt",
                "execution_mode": "shared",
                "allow_queue": true,
            }),
        )
        .await
        .unwrap();
    assert_submission_shape(&submitted.structured);
    assert_eq!(submitted.structured["requestId"], "desktop-contract-submit");
    assert_eq!(submitted.structured["executionMode"], "shared");

    let history = client
        .call_tool(
            "ptah_list_runs",
            json!({
                "session_id": session_id,
                "workspace": workspace,
            }),
        )
        .await
        .unwrap();
    assert!(!history.is_error, "{:?}", history.raw);
    let runs = history.structured["runs"].as_array().unwrap();
    assert!(
        runs.iter()
            .any(|run| run["runId"] == first_run && run["state"] == "cancelled"),
        "completed/cancelled history must remain after the live pointer moves: {runs:?}"
    );
    assert!(
        runs.iter()
            .any(|run| run["runId"] == "desktop-history-2" && run["state"] == "running"),
        "the new live run must appear alongside history: {runs:?}"
    );

    client.close_session().await.unwrap();
    handle.stop_and_wait().await;
}

async fn wait_run_not_queued(
    client: &mut McpControlClient,
    session_id: Uuid,
    workspace: &std::path::Path,
    run_id: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        let run = client
            .call_tool(
                "ptah_get_run",
                json!({
                    "session_id": session_id,
                    "workspace": workspace,
                    "run_id": run_id,
                }),
            )
            .await
            .unwrap_or_else(|error| panic!("get run {run_id}: {error}"));
        let state = run.structured["state"]
            .as_str()
            .unwrap_or("missing")
            .to_string();
        if state != "queued" || tokio::time::Instant::now() >= deadline {
            assert_ne!(state, "queued", "run {run_id} stayed queued");
            return state;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
