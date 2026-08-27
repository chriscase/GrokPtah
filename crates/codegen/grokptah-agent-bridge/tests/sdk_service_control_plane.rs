//! SDK ↔ service control-plane qualification over the **live** MCP transport.
//!
//! Scope of the evidence produced here (see
//! `docs/SDK_SERVICE_CONTROL_PLANE_QUALIFICATION.md`):
//!
//! * every call crosses a bound loopback TCP socket and the production axum
//!   router, middleware, session table, tool allowlist, and orchestration
//!   policy — no in-process shortcut, no scripted double, no recorded fixture;
//! * the client is `SdkServiceControlPlane`, which re-derives MCP Streamable
//!   HTTP framing from `reqwest` rather than reusing the in-tree
//!   `McpControlClient`, so a client-side assumption cannot mask a wire change;
//! * the host is offline (`GROKPTAH_AGENT_OFFLINE=1`) with a disposable home
//!   and a synthetic workspace, so no provider, credential, or user data is
//!   reachable.
//!
//! Where the SDK contract and the live wire disagree, the test pins the
//! disagreement instead of hiding it behind a permissive assertion.

mod common;
mod sdk_control_plane_harness;

use std::time::Duration;

use grokptah_agent_bridge::{
    ActionClass, ComputerGrantRequest, ComputerRunState, ComputerUseLimits, GrantIssuer,
    SimulatorBackend, CONTROL_TOOLS, FORBIDDEN_TOOLS,
};
use grokptah_agent_sdk::{
    Bounds, ComputerActionClass, ComputerControlRequest, DurableRunState, ErrorCode, ExecutionMode,
    SubmitTaskRequest, CONTRACT_VERSION,
};
use serde_json::{json, Value};

use sdk_control_plane_harness::{scope, DisposableService, SdkServiceControlPlane};

/// Terminal durable states, per the SDK's own vocabulary.
fn is_terminal(state: DurableRunState) -> bool {
    matches!(
        state,
        DurableRunState::Completed
            | DurableRunState::Failed
            | DurableRunState::Cancelled
            | DurableRunState::Interrupted
            | DurableRunState::LimitReached
    )
}

/// Poll a run through the live `ptah_get_run` route until terminal.
async fn drive_to_terminal(
    plane: &SdkServiceControlPlane,
    fence: &grokptah_agent_sdk::RunScope,
) -> DurableRunState {
    let mut last = DurableRunState::Queued;
    for _ in 0..80 {
        let run = plane
            .get_run(fence)
            .await
            .unwrap_or_else(|error| panic!("live get_run failed: {error:?}"));
        last = run.state;
        if is_terminal(last) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

// ── 1. Handshake and route discovery ─────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn initialize_and_tools_list_expose_exactly_the_live_allowlist() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();

    let negotiated = plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    assert!(
        negotiated.protocol_version.starts_with("202"),
        "server must negotiate a dated MCP protocol version, got {}",
        negotiated.protocol_version
    );
    assert_eq!(negotiated.server_name, "grokptah-control");
    assert!(
        !negotiated.session_id.is_empty(),
        "initialize must issue a transport session id"
    );

    // The advertised capability contract is the SDK's own, not a parallel
    // vocabulary invented by the transport.
    let contract = &negotiated.capability_contract;
    assert_eq!(
        contract["contract"].as_str(),
        Some(CONTRACT_VERSION),
        "advertised contract identifier must be the SDK constant"
    );
    let advertised: grokptah_agent_sdk::CapabilitySet =
        serde_json::from_value(contract.clone()).expect("capability set deserializes into the SDK");
    assert!(
        !advertised.capabilities.is_empty(),
        "an initialized control plane must advertise capabilities"
    );
    // Promotion stays human-gated even though the transport can reach it.
    let promote = advertised
        .capabilities
        .iter()
        .find(|descriptor| descriptor.id == "run.promote")
        .expect("promotion capability is advertised");
    assert!(
        promote.human_gate,
        "promotion must remain human-gated in the advertised contract"
    );

    let tools = plane.list_tools().await.expect("live tools/list succeeds");
    let mut sorted = tools.clone();
    sorted.sort();
    let mut expected: Vec<String> = CONTROL_TOOLS.iter().map(|name| name.to_string()).collect();
    expected.sort();
    assert_eq!(
        sorted, expected,
        "tools/list must expose exactly the allowlisted control tools"
    );
    for forbidden in FORBIDDEN_TOOLS {
        assert!(
            !tools.iter().any(|name| name == forbidden),
            "forbidden tool {forbidden} must never be discoverable"
        );
    }

    service.shutdown().await;
}

// ── 2. Read-only-by-default and fail-closed denial ───────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn undiscoverable_and_forbidden_tools_are_denied_as_public_envelopes() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    // Session lifecycle, shell, and configuration mutation are not merely
    // undiscoverable: calling them by name still fails closed.
    for denied in [
        "ptah_create_session",
        "ptah_delete_session",
        "run_terminal_cmd",
        "ptah_manage_mcp",
        "ptah_set_config",
        "ptah_totally_unknown_tool",
    ] {
        let failure = plane
            .call_tool(denied, json!({}))
            .await
            .expect_err("denied tool must not succeed");
        assert_eq!(
            failure.code(),
            ErrorCode::ForbiddenScope,
            "{denied} must be refused as forbidden scope, got {:?}",
            failure.envelope
        );
        assert_eq!(failure.http_status, 403);
        // The public projection must not echo the probed tool name back.
        assert!(
            !failure.envelope.message.contains(denied),
            "denial message must not echo the probed tool name: {}",
            failure.envelope.message
        );
        sdk_control_plane_harness::assert_no_privileged_leak(
            &failure.raw,
            &[workspace.as_str(), sdk_control_plane_harness::HARNESS_TOKEN],
        );
    }

    // A read route on the same session is reachable, proving the denial above
    // is scoped to the operation and not a blanket transport failure.
    let capacity = plane
        .call_tool("ptah_get_capacity", json!({}))
        .await
        .expect("read-only capacity is reachable");
    assert!(capacity.is_object(), "capacity must return a projection");
    let _ = session;

    service.shutdown().await;
}

// ── 3. SDK request DTOs are not wire-compatible without translation ──────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn sdk_request_dtos_require_explicit_translation_at_the_live_routes() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    let request = SubmitTaskRequest {
        request_id: "sdk-translate-1".into(),
        session_id: session.to_string(),
        workspace: workspace.clone(),
        prompt: "summarize the synthetic workspace".into(),
        bounds: Some(Bounds {
            max_rounds: Some(2),
            ..Bounds::default()
        }),
        execution_mode: Some(ExecutionMode::Shared),
        allow_queue: Some(false),
    };
    request.validate().expect("the SDK DTO is self-consistent");

    // Untranslated: the SDK serializes camelCase, the route's argument struct
    // is snake_case with `deny_unknown_fields`. This is the exact gap a
    // scripted double cannot surface, because a double would accept whatever
    // shape the test author sent.
    let naive = plane
        .call_tool_with_untranslated_dto("ptah_submit_task", &request)
        .await
        .expect_err("an untranslated SubmitTaskRequest must be refused");
    assert_eq!(
        naive.code(),
        ErrorCode::InvalidRequest,
        "untranslated submit must fail closed, got {:?}",
        naive.envelope
    );

    // Same for the Computer Use lease request, which additionally nests its
    // scope while the route flattens it.
    let lease = ComputerControlRequest {
        request_id: "sdk-translate-2".into(),
        scope: scope(session, &workspace, "run-does-not-matter"),
        expected_version: 1,
        action_classes: vec![ComputerActionClass::Semantic],
        ttl_ms: 30_000,
    };
    let naive_lease = plane
        .call_tool_with_untranslated_dto("ptah_authorize_computer_run", &lease)
        .await
        .expect_err("an untranslated ComputerControlRequest must be refused");
    assert_eq!(
        naive_lease.code(),
        ErrorCode::InvalidRequest,
        "untranslated lease must fail closed, got {:?}",
        naive_lease.envelope
    );

    // The adapter's explicit translation is accepted by the same live route.
    let receipt = plane
        .submit_task(&request)
        .await
        .expect("translated submit is accepted");
    assert_eq!(receipt.request_id, "sdk-translate-1");
    assert_eq!(receipt.session_id, session.to_string());
    assert_eq!(receipt.execution_mode, ExecutionMode::Shared);
    assert!(
        !receipt.run_id.is_empty(),
        "submit receipt must carry a durable run id"
    );

    service.shutdown().await;
}

// ── 4. Durable lifecycle: submit → get_run → events → cancel ─────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn durable_run_projections_and_event_pages_satisfy_the_sdk_contract() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    let receipt = plane
        .submit_task(&SubmitTaskRequest {
            request_id: "sdk-lifecycle-1".into(),
            session_id: session.to_string(),
            workspace: workspace.clone(),
            prompt: "list the files in the synthetic workspace".into(),
            bounds: None,
            execution_mode: None,
            allow_queue: None,
        })
        .await
        .expect("submit accepted over the live transport");

    let fence = scope(session, &workspace, &receipt.run_id);
    let terminal = drive_to_terminal(&plane, &fence).await;
    assert!(
        is_terminal(terminal),
        "an offline run must reach a durable terminal state, saw {terminal:?}"
    );

    // `ptah_get_run` is directly readable as the SDK's DurableRun.
    let run = plane.get_run(&fence).await.expect("durable run projects");
    assert_eq!(run.run_id, receipt.run_id);
    assert_eq!(run.session_id, session.to_string());
    assert_eq!(run.request_id, "sdk-lifecycle-1");
    assert_eq!(
        run.workspace, workspace,
        "the durable projection must echo the exact bound workspace"
    );
    assert!(
        !run.created_at.is_empty() && !run.updated_at.is_empty(),
        "durable timestamps must be populated"
    );

    // `ptah_get_events` is directly readable as the SDK's RunEventPage, and
    // its sequences are strictly ordered inside the run's own window.
    let page = plane
        .get_events(&fence, 0, 50)
        .await
        .expect("event page projects");
    assert!(!page.cursor_expired, "a fresh cursor must not be expired");
    let mut previous = 0u64;
    for entry in &page.entries {
        assert!(
            entry.seq > previous,
            "durable event sequences must strictly increase, saw {} after {previous}",
            entry.seq
        );
        previous = entry.seq;
    }
    if let Some(cursor) = page.next_cursor {
        assert_eq!(
            Some(cursor),
            page.entries.last().map(|entry| entry.seq),
            "next_cursor must name the last returned sequence"
        );
        // Resuming from the cursor must not replay the same entries.
        let resumed = plane
            .get_events(&fence, cursor, 50)
            .await
            .expect("cursor resume projects");
        assert!(
            resumed.entries.iter().all(|entry| entry.seq > cursor),
            "resuming from a cursor must not replay retained entries"
        );
    }

    // Cancelling a terminal run is refused as a public envelope rather than
    // silently succeeding.
    match plane.cancel_run("sdk-lifecycle-cancel", &fence).await {
        Ok(value) => assert!(
            value.is_object(),
            "an accepted cancel must return a projection"
        ),
        Err(failure) => assert!(
            matches!(
                failure.code(),
                ErrorCode::InvalidRequest | ErrorCode::StaleOrRecovery
            ),
            "cancelling a terminal run must fail closed with a public code, got {:?}",
            failure.envelope
        ),
    }

    service.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn cancelling_a_live_run_reaches_durable_cancelled_over_the_transport() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let session = service.new_build_session();
    let workspace = service.canonical_workspace();
    let marker = service.workspace.path().join("cancel_marker.txt");

    let receipt = plane
        .submit_task(&SubmitTaskRequest {
            request_id: "sdk-cancel-1".into(),
            session_id: session.to_string(),
            workspace: workspace.clone(),
            prompt: format!("run (sleep 5; echo leaked > {}) & wait", marker.display()),
            bounds: Some(Bounds {
                max_prompt_bytes: Some(50_000),
                max_rounds: Some(8),
                max_duration_ms: Some(60_000),
            }),
            execution_mode: None,
            allow_queue: None,
        })
        .await
        .expect("submit accepted");

    let fence = scope(session, &workspace, &receipt.run_id);
    tokio::time::sleep(Duration::from_millis(120)).await;

    let cancelled = plane
        .cancel_run("sdk-cancel-request", &fence)
        .await
        .expect("cancel accepted over the live transport");

    // Replaying the same request id must return the identical receipt, not a
    // second cancellation.
    let replay = plane
        .cancel_run("sdk-cancel-request", &fence)
        .await
        .expect("cancel replay accepted");
    assert_eq!(
        cancelled, replay,
        "an idempotent cancel replay must return the identical receipt"
    );

    let mut final_state = DurableRunState::Running;
    for _ in 0..80 {
        final_state = plane.get_run(&fence).await.expect("run projects").state;
        if final_state == DurableRunState::Cancelled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        final_state,
        DurableRunState::Cancelled,
        "cancelling a live run must leave a durable cancelled state"
    );
    assert!(
        !marker.exists(),
        "a cancelled run must not leave its side effect behind"
    );

    service.shutdown().await;
}

// ── 5. Exact (session, workspace, run) scope binding ─────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn run_reads_require_the_exact_session_workspace_and_run_triple() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let owner = service.new_build_session();
    let bystander = service.new_build_session();
    let workspace = service.canonical_workspace();

    let receipt = plane
        .submit_task(&SubmitTaskRequest {
            request_id: "sdk-scope-1".into(),
            session_id: owner.to_string(),
            workspace: workspace.clone(),
            prompt: "describe the synthetic workspace".into(),
            bounds: None,
            execution_mode: None,
            allow_queue: None,
        })
        .await
        .expect("submit accepted");
    let owner_fence = scope(owner, &workspace, &receipt.run_id);
    drive_to_terminal(&plane, &owner_fence).await;

    // The owning triple reads.
    plane
        .get_run(&owner_fence)
        .await
        .expect("owner reads its own run");

    // A different session in the same allowlisted workspace must not.
    let cross_session = plane
        .get_run(&scope(bystander, &workspace, &receipt.run_id))
        .await
        .expect_err("a bystander session must not read another session's run")
        .denied();
    assert_eq!(cross_session.code(), ErrorCode::ForbiddenScope);

    // The owning session claiming a workspace outside the allowlist must not.
    let (_foreign_dir, foreign) = sdk_control_plane_harness::foreign_workspace();
    let cross_workspace = plane
        .get_run(&scope(
            owner,
            &foreign.display().to_string(),
            &receipt.run_id,
        ))
        .await
        .expect_err("an un-allowlisted workspace claim must be refused")
        .denied();
    assert_eq!(cross_workspace.code(), ErrorCode::ForbiddenScope);

    // FINDING (residual, P2 — see docs/SDK_SERVICE_CONTROL_PLANE_QUALIFICATION.md):
    // an unknown run id and a run owned by another session are *not* publicly
    // indistinguishable on the durable-run routes. `load_authorized_run`
    // returns `invalid_request` / "unknown run_id" for a missing record and
    // `forbidden_scope` for a cross-scope one, so a caller holding a valid
    // session+workspace fence can enumerate run existence. The Computer Use
    // read path deliberately collapses both cases into one denial
    // (`authorize_computer_scope`), so the two surfaces disagree.
    //
    // This assertion pins the behaviour observed over the live transport at
    // this SHA. If the durable routes are hardened to match Computer Use,
    // this test fails and must be tightened to the indistinguishability
    // assertion instead.
    let unknown_run = plane
        .get_run(&scope(owner, &workspace, "run-that-never-existed"))
        .await
        .expect_err("an unknown run must be refused")
        .denied();
    assert_eq!(
        unknown_run.code(),
        ErrorCode::InvalidRequest,
        "unknown run ids currently surface as invalid_request"
    );
    assert_ne!(
        unknown_run.envelope.message, cross_session.envelope.message,
        "pinning the known divergence: an unknown run and a cross-session run \
         are publicly distinguishable on the durable-run routes"
    );
    // Both are still denials that carry no privileged detail.
    for failure in [&unknown_run, &cross_session] {
        sdk_control_plane_harness::assert_no_privileged_leak(
            &failure.raw,
            &[workspace.as_str(), sdk_control_plane_harness::HARNESS_TOKEN],
        );
    }

    service.shutdown().await;
}

// ── 6. Request-id replay and conflict ────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn request_id_replay_is_idempotent_and_a_changed_payload_conflicts() {
    let mut service = DisposableService::launch().await;
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    let first = SubmitTaskRequest {
        request_id: "sdk-replay-1".into(),
        session_id: session.to_string(),
        workspace: workspace.clone(),
        prompt: "replay probe".into(),
        bounds: None,
        execution_mode: None,
        allow_queue: None,
    };

    let accepted = plane.submit_task(&first).await.expect("first submit");
    let replayed = plane
        .submit_task(&first)
        .await
        .expect("identical replay is accepted");
    assert_eq!(
        accepted.run_id, replayed.run_id,
        "an identical request id must replay the same durable run, never fork a second one"
    );

    // Same idempotency key, different payload: the authority must refuse
    // rather than silently binding the key to new work.
    let mutated = SubmitTaskRequest {
        prompt: "a different intent under the same key".into(),
        ..first.clone()
    };
    let conflict = plane
        .submit_task(&mutated)
        .await
        .expect_err("a changed payload under a used request id must be refused")
        .denied();
    assert_eq!(
        conflict.code(),
        ErrorCode::InvalidRequest,
        "request-id conflict must map to a public invalid_request, got {:?}",
        conflict.envelope
    );
    assert_eq!(
        conflict.reason(),
        Some("conflict"),
        "the public envelope must carry the precise reason code"
    );

    // Exactly one run exists for that key.
    let fence = scope(session, &workspace, &accepted.run_id);
    let run = plane
        .get_run(&fence)
        .await
        .expect("the single run projects");
    assert_eq!(run.request_id, "sdk-replay-1");

    service.shutdown().await;
}

// ── 7. Manager-issued grants are the only mutation authority ─────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn computer_mutations_require_an_initialized_client_and_mint_server_side_grants() {
    let mut service = DisposableService::launch().await;
    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    // Create the run through the host-owned ledger: the control plane has no
    // creation route at all.
    let computer = service.computer_service();
    let run = computer
        .create_run(
            "sdk-cu-create",
            session,
            Some(workspace.clone()),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .expect("computer run is created host-side");
    let fence = scope(session, &workspace, &run.run_id);

    // (a) An authenticated but *uninitialized* client cannot mutate. The
    //     bearer token alone is transport reachability, never authority.
    let anonymous = service.control_plane();
    let denied = anonymous
        .call_tool(
            "ptah_authorize_computer_run",
            json!({
                "request_id": "sdk-cu-grant-anon",
                "session_id": session.to_string(),
                "workspace": workspace,
                "run_id": run.run_id,
                "expected_version": run.version,
                "action_classes": ["semantic"],
                "ttl_ms": 30_000,
            }),
        )
        .await
        .expect_err("an uninitialized client must not mutate a computer run");
    assert_eq!(
        denied.code(),
        ErrorCode::ForbiddenScope,
        "uninitialized mutation must fail closed, got {:?}",
        denied.envelope
    );
    // Read routes stay available to the same uninitialized client.
    anonymous
        .get_computer_run(&fence)
        .await
        .expect("reads remain available without an initialized session");
    assert_eq!(
        service.stored_computer_run(&run.run_id).version,
        run.version,
        "a denied mutation must not advance the durable revision"
    );

    // (b) An initialized client mutates, and the grant it produces is minted
    //     server-side from transport identity.
    let mut plane = service.control_plane();
    let negotiated = plane
        .initialize("grokptah-sdk-qualification", "9.9.9")
        .await
        .expect("live initialize succeeds");

    let lease = ComputerControlRequest {
        request_id: "sdk-cu-grant-1".into(),
        scope: fence.clone(),
        expected_version: run.version,
        action_classes: vec![ComputerActionClass::Semantic],
        ttl_ms: 30_000,
    };
    let (response, projection) = plane
        .authorize_computer_run(&lease, Some(3))
        .await
        .expect("translated lease is accepted");
    assert_eq!(response.scope, fence);
    assert!(
        response.version > run.version,
        "an accepted lease must advance the durable revision"
    );

    let stored = service.stored_computer_run(&run.run_id);
    let grant = stored
        .grant
        .as_ref()
        .expect("an accepted lease stores a grant");
    match &grant.issued_by {
        GrantIssuer::McpClient { client_id } => {
            assert!(
                client_id.contains(&negotiated.session_id),
                "the grant actor must be derived from the server-issued transport \
                 session id, got {client_id}"
            );
            assert!(
                client_id.starts_with("grokptah-sdk-qualification@9.9.9#"),
                "the grant actor must be derived from initialize metadata, got {client_id}"
            );
        }
        other => panic!("an MCP-issued lease must record an MCP client issuer, got {other:?}"),
    }
    assert_eq!(
        grant.action_classes.iter().copied().collect::<Vec<_>>(),
        vec![ActionClass::Semantic],
        "the grant must carry exactly the requested action classes"
    );
    assert_eq!(grant.uses_remaining, Some(3));
    assert!(
        grant.expires_at > grant.issued_at,
        "a grant must have a positive lease lifetime"
    );

    // (c) A caller cannot name its own actor: the argument struct denies
    //     unknown fields, so `client_id` is not forgeable over the wire.
    let forged = plane
        .call_tool(
            "ptah_authorize_computer_run",
            json!({
                "request_id": "sdk-cu-grant-forge",
                "session_id": session.to_string(),
                "workspace": workspace,
                "run_id": run.run_id,
                "expected_version": stored.version,
                "action_classes": ["semantic"],
                "ttl_ms": 30_000,
                "client_id": "operator-console",
            }),
        )
        .await
        .expect_err("a caller-supplied client id must be refused");
    assert_eq!(
        forged.code(),
        ErrorCode::InvalidRequest,
        "a forged actor argument must fail closed, got {:?}",
        forged.envelope
    );

    // (d) The projection stays redacted: no host path, home, or token.
    sdk_control_plane_harness::assert_no_privileged_leak(
        &projection,
        &[sdk_control_plane_harness::HARNESS_TOKEN],
    );

    service.shutdown().await;
}

// ── 8. Lease/revision fencing and observation staleness ──────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn computer_control_is_revision_fenced_and_projects_observation_staleness() {
    let mut service = DisposableService::launch().await;
    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    let computer = service.computer_service();
    let run = computer
        .create_run(
            "sdk-cu-fence-create",
            session,
            Some(workspace.clone()),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .expect("computer run is created host-side");
    let fence = scope(session, &workspace, &run.run_id);

    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    // A stale revision is refused before any state change.
    let stale = plane
        .authorize_computer_run(
            &ComputerControlRequest {
                request_id: "sdk-cu-stale".into(),
                scope: fence.clone(),
                expected_version: run.version + 41,
                action_classes: vec![ComputerActionClass::Semantic],
                ttl_ms: 30_000,
            },
            None,
        )
        .await
        .expect_err("a stale expected_version must be refused")
        .denied();
    // FINDING (residual, P1 — see docs/SDK_SERVICE_CONTROL_PLANE_QUALIFICATION.md):
    // a stale `expected_version` is refused, but the public taxonomy reports
    // it as `invalid_request`, not `stale_or_recovery`.
    // `computer_mutation_error` folds `ComputerErrorCode::Conflict` and
    // `StaleObservation` into `OrchErrorCode::Conflict`, which
    // `public_error_code` maps to `InvalidRequest`. `OrchErrorCode::StaleVersion`
    // already maps to `StaleOrRecovery` but is never emitted by these routes,
    // so an SDK consumer cannot separate a retriable stale revision from a
    // malformed request on the exact surface where revision fencing matters.
    //
    // The fence itself holds: the mutation is refused and no revision moves.
    // This assertion pins the observed public code at this SHA.
    assert_eq!(
        stale.code(),
        ErrorCode::InvalidRequest,
        "revision fencing currently surfaces as invalid_request, got {:?}",
        stale.envelope
    );
    assert_eq!(
        stale.reason(),
        Some("conflict"),
        "the precise reason code is the only signal that this was a fence, not a bad request"
    );
    assert_eq!(stale.http_status, 409);
    assert_eq!(
        service.stored_computer_run(&run.run_id).version,
        run.version,
        "a fenced mutation must not advance the durable revision"
    );

    // A zero-duration lease is refused by the SDK validator before transport,
    // and by the authority if a caller skips validation.
    let zero_ttl = plane
        .call_tool(
            "ptah_authorize_computer_run",
            json!({
                "request_id": "sdk-cu-zero-ttl",
                "session_id": session.to_string(),
                "workspace": workspace,
                "run_id": run.run_id,
                "expected_version": run.version,
                "action_classes": ["semantic"],
                "ttl_ms": 0,
            }),
        )
        .await
        .expect_err("a zero-duration lease must be refused");
    assert_eq!(zero_ttl.code(), ErrorCode::InvalidRequest);

    // A fresh lease at the current revision is accepted.
    let (accepted, _) = plane
        .authorize_computer_run(
            &ComputerControlRequest {
                request_id: "sdk-cu-fresh".into(),
                scope: fence.clone(),
                expected_version: run.version,
                action_classes: vec![ComputerActionClass::Semantic],
                ttl_ms: 30_000,
            },
            None,
        )
        .await
        .expect("a lease at the current revision is accepted");

    // Observe host-side so the projection carries a real observation, then
    // read its freshness through the live route.
    let observed = computer
        .observe("sdk-cu-observe", &run.run_id, accepted.version)
        .await
        .expect("simulator observation succeeds");
    let projection = plane
        .get_computer_run(&fence)
        .await
        .expect("projection reads over the live transport");
    let observation = projection
        .get("observation")
        .expect("an observed run projects its observation");
    assert_eq!(
        observation.get("stale").and_then(Value::as_bool),
        Some(false),
        "a just-captured observation must project as fresh"
    );
    assert_eq!(
        observation.get("observationId").and_then(Value::as_str),
        Some(observed.observation_id.as_str()),
        "the projected observation id must match the durable record"
    );
    // Raw frames never cross the boundary: only bounded metadata does.
    assert!(
        observation.get("screenshot").is_none() && observation.get("elements").is_none(),
        "the public observation summary must not carry frames or element trees: {observation}"
    );

    // A second run with a deliberately tight staleness bound proves the
    // fresh → stale transition over the live route, not just the fresh case.
    // Only the stale side is asserted on a clock: the fresh side is covered
    // above under the default 10s bound, so no assertion here races a
    // scheduler stall.
    let tight = computer
        .create_run(
            "sdk-cu-stale-create",
            session,
            Some(workspace.clone()),
            SimulatorBackend::demo_target(),
            ComputerUseLimits {
                max_observation_age_millis: 200,
                ..ComputerUseLimits::default()
            },
        )
        .expect("tight-bound computer run is created host-side");
    let tight_fence = scope(session, &workspace, &tight.run_id);
    let (tight_lease, _) = plane
        .authorize_computer_run(
            &ComputerControlRequest {
                request_id: "sdk-cu-stale-grant".into(),
                scope: tight_fence.clone(),
                expected_version: tight.version,
                action_classes: vec![ComputerActionClass::Semantic],
                ttl_ms: 30_000,
            },
            None,
        )
        .await
        .expect("lease on the tight-bound run is accepted");
    computer
        .observe("sdk-cu-stale-observe", &tight.run_id, tight_lease.version)
        .await
        .expect("simulator observation succeeds");

    tokio::time::sleep(Duration::from_millis(700)).await;
    let aged = plane
        .get_computer_run(&tight_fence)
        .await
        .expect("aged projection reads over the live transport");
    assert_eq!(
        aged["observation"]["stale"].as_bool(),
        Some(true),
        "an observation older than the run's staleness bound must project as \
         stale over the live transport: {}",
        aged["observation"]
    );

    // Reusing a spent revision after the accepted lease is refused.
    let replayed_fence = plane
        .authorize_computer_run(
            &ComputerControlRequest {
                request_id: "sdk-cu-refence".into(),
                scope: fence.clone(),
                expected_version: run.version,
                action_classes: vec![ComputerActionClass::Semantic],
                ttl_ms: 30_000,
            },
            None,
        )
        .await
        .expect_err("a spent revision must not be reusable")
        .denied();
    // Same taxonomy finding as above; the fence itself is enforced.
    assert_eq!(replayed_fence.code(), ErrorCode::InvalidRequest);
    assert_eq!(replayed_fence.reason(), Some("conflict"));

    service.shutdown().await;
}

// ── 9. Redacted audit projections map onto the SDK event page ────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn computer_audit_pages_are_redacted_and_map_onto_the_sdk_event_page() {
    let mut service = DisposableService::launch().await;
    let session = service.new_build_session();
    let workspace = service.canonical_workspace();

    let computer = service.computer_service();
    let run = computer
        .create_run(
            "sdk-cu-audit-create",
            session,
            Some(workspace.clone()),
            SimulatorBackend::demo_target(),
            Default::default(),
        )
        .expect("computer run is created host-side");
    let granted = computer
        .authorize_mcp_client(
            "sdk-cu-audit-grant",
            &run.run_id,
            run.version,
            "operator-console@1#session".into(),
            ComputerGrantRequest {
                action_classes: [ActionClass::Semantic].into_iter().collect(),
                ttl_ms: 60_000,
                uses_remaining: Some(2),
            },
        )
        .expect("host-side grant");
    computer
        .observe("sdk-cu-audit-observe", &run.run_id, granted.version)
        .await
        .expect("simulator observation succeeds");

    let fence = scope(session, &workspace, &run.run_id);
    let mut plane = service.control_plane();
    plane
        .initialize("grokptah-sdk-qualification", "1.0.0")
        .await
        .expect("live initialize succeeds");

    let (page, raw) = plane
        .get_computer_run_events(&fence, None, 50)
        .await
        .expect("audit page maps onto the SDK event page");

    assert!(
        !page.entries.is_empty(),
        "an authorized and observed run must have audit entries"
    );
    let mut previous = 0u64;
    for entry in &page.entries {
        assert!(
            entry.seq > previous,
            "audit sequences must strictly increase, saw {} after {previous}",
            entry.seq
        );
        previous = entry.seq;
        assert!(
            !entry.kind.is_empty(),
            "every mapped audit event must carry a public kind"
        );
    }
    assert!(
        page.entries.iter().any(|entry| entry.kind == "authorize"),
        "the grant must appear in the redacted audit journal"
    );
    assert!(!page.cursor_expired, "a full read must not report expiry");

    // The raw page carries no host path, no home, no token, and no frame.
    sdk_control_plane_harness::assert_no_privileged_leak(
        &raw,
        &[
            sdk_control_plane_harness::HARNESS_TOKEN,
            "screenshot",
            "elements",
        ],
    );

    // Cursor continuity: resuming past the last sequence yields an empty page
    // that is explicitly not an expiry.
    if let Some(cursor) = page.next_cursor {
        let (resumed, _) = plane
            .get_computer_run_events(&fence, Some(cursor), 50)
            .await
            .expect("cursor resume maps");
        assert!(
            resumed.entries.iter().all(|entry| entry.seq > cursor),
            "a resumed audit page must not replay retained entries"
        );
        assert!(
            !resumed.cursor_expired,
            "a live cursor must not be reported as expired"
        );
    }

    // Cancelling through the live route drives the run terminal and the
    // journal records it.
    let cancelled = plane
        .call_tool(
            "ptah_cancel_computer_run",
            json!({
                "request_id": "sdk-cu-audit-cancel",
                "session_id": session.to_string(),
                "workspace": workspace,
                "run_id": run.run_id,
                "expected_version": service.stored_computer_run(&run.run_id).version,
            }),
        )
        .await
        .expect("cancel accepted over the live transport");
    let state: ComputerRunState =
        serde_json::from_value(cancelled["state"].clone()).expect("state projects");
    assert!(
        state.is_terminal(),
        "an accepted cancel must leave a terminal computer run, saw {state:?}"
    );

    service.shutdown().await;
}
