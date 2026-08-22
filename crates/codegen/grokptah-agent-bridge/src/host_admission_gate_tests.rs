//! Desktop Build must use the same host-owned route/purpose gate as
//! readiness and orchestration Execution.

use super::*;
use crate::discover::{home_override_serial, set_grokptah_home_override};
use crate::events::SessionUpdate;
use crate::gateway_config::{
    save_managed_profile_capabilities, CapabilitySource, ModelCapabilities, ProviderDeadlineClass,
    ProviderDialect, ProviderKind, ProviderModel, ProviderProfile, CAPABILITY_QUALIFICATION_SCHEMA,
    XAI_PROVIDER_ID,
};
use crate::native_coding_readiness::DESKTOP_OWNER_ID;
use crate::orchestration::{
    AuthContext, OrchError, OrchErrorCode, OrchestrationConfig, OrchestrationService, RunBounds,
    WorkspaceAllowlist,
};
use crate::orchestration::{
    QuotaClass, QuotaLimits, QuotaReservation, DEFAULT_MAX_IN_FLIGHT_RESERVATIONS,
};
use axum::body::Body;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct IsolatedHome {
    _tmp: tempfile::TempDir,
    _lock: std::sync::MutexGuard<'static, ()>,
    previous_env: Vec<(&'static str, Option<std::ffi::OsString>)>,
}

impl IsolatedHome {
    fn enter() -> Self {
        let lock = home_override_serial();
        let tmp = tempfile::tempdir().expect("test home");
        let home = tmp.path().join(".grokptah");
        std::fs::create_dir_all(home.join("sessions")).expect("sessions directory");
        set_grokptah_home_override(Some(home));
        Self {
            _tmp: tmp,
            _lock: lock,
            previous_env: Vec::new(),
        }
    }

    fn set(&mut self, key: &'static str, value: &str) {
        self.remember(key);
        // SAFETY: tests hold `home_override_serial` and restore these keys on drop.
        unsafe {
            std::env::set_var(key, value);
        }
    }

    fn remove(&mut self, key: &'static str) {
        self.remember(key);
        // SAFETY: see `set`; this restores the process-wide test boundary on drop.
        unsafe {
            std::env::remove_var(key);
        }
    }

    fn remember(&mut self, key: &'static str) {
        if self
            .previous_env
            .iter()
            .any(|(existing, _)| *existing == key)
        {
            return;
        }
        self.previous_env.push((key, std::env::var_os(key)));
    }
}

impl Drop for IsolatedHome {
    fn drop(&mut self) {
        set_grokptah_home_override(None);
        for (key, previous) in self.previous_env.drain(..).rev() {
            // SAFETY: see IsolatedHome::set; this restores the exact process state.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

async fn spawn_xai_sse() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/v1/chat/completions", post(xai_sse_ok))
        .with_state(requests.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{address}/v1"), requests, server)
}

async fn xai_sse_ok(State(requests): State<Arc<AtomicUsize>>) -> Response {
    requests.fetch_add(1, Ordering::SeqCst);
    let chunks = vec![
        Ok::<_, std::io::Error>(Bytes::from_static(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
        )),
        Ok(Bytes::from_static(
            b"data: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
        )),
        Ok(Bytes::from_static(b"data: [DONE]\n\n")),
    ];
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(futures::stream::iter(chunks)))
        .unwrap()
}

fn measured_coding_model(id: &str) -> ProviderModel {
    let mut model = ProviderModel::unqualified(id);
    model.capabilities = ModelCapabilities {
        chat: true,
        tools: true,
        stream: true,
        parallel_tool_calls: true,
        source: CapabilitySource::Measured,
        qualification_schema: Some(CAPABILITY_QUALIFICATION_SCHEMA.into()),
        ..ModelCapabilities::default()
    };
    model
}

fn managed_xai_profile(base_url: &str, model: ProviderModel) -> ProviderProfile {
    ProviderProfile {
        id: XAI_PROVIDER_ID.into(),
        label: "xAI".into(),
        kind: ProviderKind::Xai,
        dialect: ProviderDialect::XaiChatCompletions,
        deadline_class: ProviderDeadlineClass::Standard,
        base_url: base_url.into(),
        credential_ref: Some("managed:xai:api-key".into()),
        models: vec![model],
        managed_by_env: false,
        managed_by_host: true,
    }
}

fn save_measured_history(base_url: &str, model_id: &str) {
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model_id)
        .unwrap()
        .expect("xAI credential");
    let fingerprint = credentials.qualification_identity_fingerprint();
    let model = measured_coding_model(model_id);
    let profile = managed_xai_profile(base_url, model.clone());
    save_managed_profile_capabilities(&profile, &model, &fingerprint).unwrap();
}

fn xai_host(model_id: &str) -> (AgentHostHandle, Uuid, tempfile::TempDir) {
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.set_model(model_id.into());
    host.start().expect("start host");
    let workspace = tempfile::tempdir().expect("workspace");
    let session = host
        .session_new_kind(SessionKind::Build)
        .expect("create build session");
    host.session_set_cwd(session.id, workspace.path())
        .expect("set cwd");
    host.ensure_session_agent(session.id)
        .expect("persistent agent");
    (host, session.id, workspace)
}

fn orch_for(host: &AgentHostHandle, workspace: &std::path::Path) -> Arc<OrchestrationService> {
    OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        host.ensure_orchestration_store().unwrap(),
        OrchestrationConfig {
            bearer_token: "p2-admission-token".into(),
            allowlist: WorkspaceAllowlist::new([workspace.to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    )
}

fn auth() -> AuthContext {
    AuthContext {
        token_id: "p2-admission-token".into(),
        owner_id: crate::native_coding_readiness::DESKTOP_OWNER_ID.into(),
    }
}

fn assert_requalify(error: &OrchError) {
    assert_eq!(error.code, OrchErrorCode::Conflict);
    assert!(
        error.message.contains("requalify"),
        "unexpected message: {}",
        error.message
    );
    assert_eq!(
        error
            .data
            .as_ref()
            .and_then(|data| data.get("reasonCode"))
            .and_then(serde_json::Value::as_str),
        Some("requalify_required")
    );
}

fn assert_no_dispatch(host: &AgentHostHandle, session_id: Uuid, requests: &AtomicUsize) {
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(host.list_session_runs(session_id).unwrap().is_empty());
    let store = host.ensure_orchestration_store().unwrap();
    assert!(store.list_provider_attempts().unwrap().is_empty());
    assert!(store.list_quota_reservations().unwrap().is_empty());
    assert!(host.run_usage_trackers.lock().is_empty());
    assert!(!host.session_has_turn_cancel(session_id));
    assert_eq!(
        host.ensure_session_agent(session_id)
            .unwrap()
            .current_run_id,
        None
    );
}

fn assert_session_unmutated(
    host: &AgentHostHandle,
    session_id: Uuid,
    title: &str,
    transcript: &[crate::session::TranscriptEntry],
    reservation: Option<&str>,
) {
    let summary = host.session_inspect(session_id).unwrap();
    assert_eq!(summary.title, title);
    let after = host.session_transcript(session_id).unwrap();
    assert_eq!(after.len(), transcript.len());
    for (got, expected) in after.iter().zip(transcript.iter()) {
        assert_eq!(got.role, expected.role);
        assert_eq!(got.text, expected.text);
    }
    assert_eq!(
        host.turn_reservation_owner(session_id).as_deref(),
        reservation
    );
    assert!(!host.session_has_turn_cancel(session_id));
}

fn drain_turn_started(rx: &mut crate::event_bus::EventReceiver) {
    while let Ok(update) = rx.try_recv() {
        assert!(
            !matches!(update, SessionUpdate::TurnStarted { .. }),
            "admission failure emitted TurnStarted"
        );
    }
}

#[test]
fn session_prompt_inner_must_admit_before_session_mutation() {
    let host_src = include_str!("host.rs");
    let inner = host_src
        .split("async fn session_prompt_inner")
        .nth(1)
        .expect("session_prompt_inner");
    let admission = inner
        .split("// ── desktop Build admission (no session mutation) ──")
        .nth(1)
        .expect("admission marker")
        .split("// ── session mutation after durable admission ──")
        .next()
        .unwrap();
    let mutation = inner
        .split("// ── session mutation after durable admission ──")
        .nth(1)
        .expect("mutation marker");
    assert!(
        admission.contains("validate_provider_route_for_purpose"),
        "desktop Build must call the shared admission validator after route capture"
    );
    assert!(
        admission.contains("RunPurpose::Execution"),
        "desktop Build must admit as Execution"
    );
    assert!(
        admission.contains("admit_desktop_build_run"),
        "desktop Build must persist the admitted Run before session mutation"
    );
    assert!(
        admission.contains("save_run_and_activate_agent_with_quota")
            || include_str!("host.rs").contains("save_run_and_activate_agent_with_quota"),
        "desktop Build must reserve quota atomically with Run+Agent activation"
    );
    assert!(
        !admission.contains("commit_admitted_turn"),
        "session mutation must not run inside the admission window"
    );
    assert!(
        !admission.contains("TurnBusyGuard"),
        "busy state must not be taken before durable admission"
    );
    assert!(
        !admission.contains("persist_session"),
        "transcript persistence must wait until after admission"
    );
    assert!(
        !admission.contains("TurnStarted"),
        "TurnStarted must wait until after admission"
    );
    assert!(
        !admission.contains("run_promotion::prepare"),
        "worktree mutation must wait until after admission"
    );
    assert!(
        mutation.contains("commit_admitted_turn"),
        "session mutation must happen only after admission"
    );
    assert!(
        inner.contains("external_provider_route"),
        "already-admitted frozen routes stay on the captured snapshot"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn declared_fallback_after_measured_xai_history_refuses_desktop_build() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-xai-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, workspace) = xai_host("grok-route");
    let orch = orch_for(&host, workspace.path());
    home.set("XAI_API_BASE", "http://127.0.0.1:1/v1");

    let readiness = host.native_coding_readiness(XAI_PROVIDER_ID, "grok-route");
    assert_eq!(
        readiness.qualification_evidence,
        crate::native_coding_readiness::QualificationEvidence::Stale
    );
    assert!(!readiness.execution.permitted);
    assert_eq!(
        readiness.execution.reason_code,
        crate::native_coding_readiness::AdmissionReasonCode::RequalifyRequired
    );
    assert!(readiness.manager_proposal.permitted);

    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    let mut events = host.subscribe_events();
    let desktop_error = host
        .session_prompt_with_max_rounds(lane_id, "must not dispatch".into(), Some(1))
        .await
        .unwrap_err();
    let orch_error = desktop_error
        .downcast_ref::<OrchError>()
        .expect("desktop Build must return the typed admission error");
    assert_requalify(orch_error);
    assert_no_dispatch(&host, lane_id, &requests);
    assert_session_unmutated(&host, lane_id, &before_title, &before_transcript, None);
    drain_turn_started(&mut events);

    let mcp_error = orch
        .submit_task(
            &auth(),
            "p2-mcp-stale",
            lane_id,
            workspace.path(),
            "must not dispatch".into(),
            None,
        )
        .await
        .unwrap_err();
    assert_requalify(&mcp_error);
    assert_no_dispatch(&host, lane_id, &requests);

    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn declared_first_use_sends_exactly_one_provider_request() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-first-use-key");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    let readiness = host.native_coding_readiness(XAI_PROVIDER_ID, "grok-route");
    assert!(readiness.execution.permitted);
    assert_eq!(
        readiness.execution.reason_code,
        crate::native_coding_readiness::AdmissionReasonCode::DeclaredFirstUse
    );
    host.session_prompt_with_max_rounds(lane_id, "say ok".into(), Some(1))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(host.list_session_runs(lane_id).unwrap().len(), 1);
    assert_eq!(
        host.ensure_orchestration_store()
            .unwrap()
            .list_quota_reservations()
            .unwrap()
            .len(),
        1
    );
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn measured_execution_remains_functional() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-measured-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    let readiness = host.native_coding_readiness(XAI_PROVIDER_ID, "grok-route");
    assert!(readiness.execution.permitted);
    assert_eq!(
        readiness.execution.reason_code,
        crate::native_coding_readiness::AdmissionReasonCode::MeasuredEligible
    );
    host.session_prompt_with_max_rounds(lane_id, "say ok".into(), Some(1))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(host.list_session_runs(lane_id).unwrap().len(), 1);
    host.stop().unwrap();
    server.abort();
}

#[test]
fn manager_proposal_tool_autodeny_is_unchanged() {
    let _home = IsolatedHome::enter();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.set_permission_mode("bypassPermissions".into());
    host.start().unwrap();
    let lane = host.session_new_kind(SessionKind::Build).unwrap();
    let agent = host.ensure_session_agent(lane.id).unwrap();
    let revision = agent.current_spec().unwrap().revision;
    let store = host.ensure_orchestration_store().unwrap();
    let now = chrono::Utc::now();
    let mut work = crate::orchestration::WorkItem::new(
        "manager-decision",
        "return a typed proposal",
        lane.id,
        agent.workspace.clone(),
        "manager-supervisor",
        crate::orchestration::WorkPolicy::default(),
    )
    .unwrap();
    work.assigned_agent_id = Some(agent.agent_id.clone());
    work.assignment_status = crate::orchestration::AssignmentStatus::Accepted;
    work.parent_work_id = Some("manager-root".into());
    work.source_manager_plan_id = Some("manager-plan".into());
    work.source_manager_step_id = Some("__manager_decision__".into());
    work.validate().unwrap();
    store.save_work_item(&work).unwrap();
    let run = RunRecord {
        run_id: "manager-proposal-run".into(),
        session_id: lane.id,
        workspace: agent.workspace.clone(),
        request_id: "manager-proposal-intent".into(),
        client_id: Some("native-executor".into()),
        state: RunState::Running,
        purpose: RunPurpose::ManagerProposal,
        provider_route: None,
        agent_id: Some(agent.agent_id.clone()),
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: Some(revision),
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds: agent.current_spec().unwrap().default_run_bounds.clone(),
        prompt_preview: "typed proposal".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: now,
        updated_at: now,
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: RunAggregates::default(),
        progress: None,
        execution: None,
        approval: None,
    };
    store
        .save_run_and_activate_agent(&run, &agent.agent_id)
        .unwrap();
    host.reserve_orchestration_turn(&run.run_id, lane.id)
        .unwrap();
    host.run_usage_trackers
        .lock()
        .insert(lane.id, RunUsageTracker::from_run(store.clone(), &run));

    assert_eq!(
        host.tool_gate(lane.id, "run_terminal_cmd"),
        ToolGate::AutoDeny
    );
    assert_eq!(host.tool_gate(lane.id, "mcp"), ToolGate::AutoDeny);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn reserved_queue_drain_preserves_reservation_on_stale_admission() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-queue-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    home.set("XAI_API_BASE", "http://127.0.0.1:1/v1");
    host.session_queue_add(lane_id, "queued stale prompt".into(), false)
        .unwrap();
    let drained = host.session_queue_take_next(lane_id).unwrap();
    let owner = drained.reservation.expect("drain reservation");
    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    let mut events = host.subscribe_events();
    let error = host
        .session_prompt_reserved_with_max_rounds(
            lane_id,
            "queued stale prompt".into(),
            Some(1),
            &owner,
        )
        .await
        .unwrap_err();
    assert_requalify(error.downcast_ref::<OrchError>().expect("typed error"));
    assert_no_dispatch(&host, lane_id, &requests);
    assert_session_unmutated(
        &host,
        lane_id,
        &before_title,
        &before_transcript,
        Some(&owner),
    );
    drain_turn_started(&mut events);
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn accepted_plan_refuses_stale_admission_without_session_effects() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-plan-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    {
        let mut g = host.inner.lock();
        let session = g.sessions.get_mut(&lane_id).unwrap();
        session.plan_steps = vec!["inspect the workspace".into()];
        session.plan_goal = Some("stay local".into());
        session.plan_mode = true;
        session.plan_status = "proposed".into();
    }
    home.set("XAI_API_BASE", "http://127.0.0.1:1/v1");
    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    let mut events = host.subscribe_events();
    let error = host.accept_plan(lane_id).await.unwrap_err();
    assert_requalify(error.downcast_ref::<OrchError>().expect("typed error"));
    assert_no_dispatch(&host, lane_id, &requests);
    assert_session_unmutated(&host, lane_id, &before_title, &before_transcript, None);
    drain_turn_started(&mut events);
    let status = host
        .inner
        .lock()
        .sessions
        .get(&lane_id)
        .unwrap()
        .plan_status
        .clone();
    assert_eq!(status, "accepted");
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn persistent_resume_refuses_stale_admission_without_new_run() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-resume-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    host.session_prompt_with_max_rounds(lane_id, "say ok".into(), Some(1))
        .await
        .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(host.list_session_runs(lane_id).unwrap().len(), 1);
    home.set("XAI_API_BASE", "http://127.0.0.1:1/v1");
    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    let mut events = host.subscribe_events();
    let error = host
        .resume_agent(lane_id, "must not resume".into(), Some(1))
        .await
        .unwrap_err();
    assert_requalify(error.downcast_ref::<OrchError>().expect("typed error"));
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(host.list_session_runs(lane_id).unwrap().len(), 1);
    assert_session_unmutated(&host, lane_id, &before_title, &before_transcript, None);
    drain_turn_started(&mut events);
    assert_eq!(
        host.ensure_session_agent(lane_id).unwrap().current_run_id,
        None
    );
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn ledger_unavailable_and_fault_cutpoints_leave_zero_effects() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-cutpoint-key");
    let (host, lane_id, _workspace) = xai_host("grok-route");
    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    for cutpoint in [
        DesktopAdmissionCutpoint::AfterRouteCapture,
        DesktopAdmissionCutpoint::AfterValidation,
        DesktopAdmissionCutpoint::BeforePersist,
        DesktopAdmissionCutpoint::AfterPersistBeforeSessionCommit,
        DesktopAdmissionCutpoint::LedgerUnavailable,
    ] {
        let mut events = host.subscribe_events();
        host.set_desktop_admission_cutpoint(Some(cutpoint));
        assert!(host
            .session_prompt_with_max_rounds(lane_id, "must not dispatch".into(), Some(1))
            .await
            .is_err());
        assert_no_dispatch(&host, lane_id, &requests);
        assert_session_unmutated(&host, lane_id, &before_title, &before_transcript, None);
        drain_turn_started(&mut events);
    }
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn quota_exhausted_refuses_desktop_build_without_session_effects() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-quota-key");
    let (host, lane_id, workspace) = xai_host("grok-route");
    let agent = host.ensure_session_agent(lane_id).unwrap();
    let spec = agent.current_spec().unwrap().clone();
    let workspace_key = dunce::canonicalize(workspace.path())
        .unwrap_or_else(|_| workspace.path().to_path_buf())
        .display()
        .to_string();
    let route = host
        .capture_provider_route(&spec.model.selection_key, host.current_effort())
        .unwrap();
    let store = host.ensure_orchestration_store().unwrap();
    let now = chrono::Utc::now();
    for index in 0..DEFAULT_MAX_IN_FLIGHT_RESERVATIONS {
        let run_id = format!("seed-quota-{index}");
        let sealed = route
            .clone()
            .bind_quota(QuotaClass::CodingExecution, format!("quota-{run_id}"))
            .unwrap();
        let run = RunRecord {
            run_id: run_id.clone(),
            session_id: lane_id,
            workspace: workspace_key.clone(),
            request_id: format!("seed-quota-req-{index}"),
            client_id: Some("desktop".into()),
            state: RunState::Running,
            purpose: RunPurpose::Execution,
            provider_route: Some(sealed),
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
                max_total_tokens: Some(8_000),
                ..RunBounds::default()
            },
            prompt_preview: "seed".into(),
            start_seq: Some(1),
            end_seq: None,
            created_at: now,
            updated_at: now,
            terminal_result: None,
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: RunAggregates::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        let reservation =
            QuotaReservation::for_run(&run, DESKTOP_OWNER_ID, QuotaLimits::default(), now).unwrap();
        store.save_run_with_quota(&run, &reservation).unwrap();
    }
    let before_title = host.session_inspect(lane_id).unwrap().title;
    let before_transcript = host.session_transcript(lane_id).unwrap();
    let mut events = host.subscribe_events();
    let error = host
        .session_prompt_with_max_rounds(lane_id, "must not dispatch".into(), Some(1))
        .await
        .unwrap_err();
    let orch = error
        .downcast_ref::<OrchError>()
        .expect("quota exhaustion must be a typed orchestration error");
    assert_eq!(orch.code, OrchErrorCode::CapacityExhausted);
    assert_eq!(requests.load(Ordering::SeqCst), 0);
    assert!(host
        .list_session_runs(lane_id)
        .unwrap()
        .iter()
        .all(|run| run.run_id.starts_with("seed-quota-")));
    assert_eq!(
        store.list_quota_reservations().unwrap().len(),
        DEFAULT_MAX_IN_FLIGHT_RESERVATIONS as usize
    );
    assert_session_unmutated(&host, lane_id, &before_title, &before_transcript, None);
    drain_turn_started(&mut events);
    assert_eq!(
        host.ensure_session_agent(lane_id).unwrap().current_run_id,
        None
    );
    host.stop().unwrap();
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn already_service_admitted_frozen_route_does_not_double_reserve() {
    let (base_url, requests, server) = spawn_xai_sse().await;
    let mut home = IsolatedHome::enter();
    home.remove("GROKPTAH_AGENT_OFFLINE");
    home.set("XAI_API_BASE", &base_url);
    home.set("XAI_API_KEY", "synthetic-p2-frozen-key");
    save_measured_history(&base_url, "grok-route");
    let (host, lane_id, workspace) = xai_host("grok-route");
    let agent = host.ensure_session_agent(lane_id).unwrap();
    let spec = agent.current_spec().unwrap().clone();
    let workspace_key = dunce::canonicalize(workspace.path())
        .unwrap_or_else(|_| workspace.path().to_path_buf())
        .display()
        .to_string();
    let route = host
        .capture_provider_route(&spec.model.selection_key, host.current_effort())
        .unwrap()
        .bind_quota(QuotaClass::CodingExecution, "quota-frozen-external")
        .unwrap();
    let now = chrono::Utc::now();
    let mut bounds = spec.default_run_bounds.clone();
    bounds.max_total_tokens = bounds
        .max_total_tokens
        .or(Some(DEFAULT_PERSISTENT_AGENT_MAX_TOTAL_TOKENS));
    let run = RunRecord {
        run_id: "service-admitted-run".into(),
        session_id: lane_id,
        workspace: workspace_key,
        request_id: "service-admitted-req".into(),
        client_id: Some("mcp".into()),
        state: RunState::Running,
        purpose: RunPurpose::Execution,
        provider_route: Some(route),
        agent_id: Some(agent.agent_id.clone()),
        retry_of: None,
        parent_run_id: None,
        agent_spec_revision: Some(spec.revision),
        checkpoint_id: None,
        continuation_context_id: None,
        continuation_context_hash: None,
        continuation_fidelity: None,
        queue_position: None,
        bounds,
        prompt_preview: "frozen".into(),
        start_seq: Some(1),
        end_seq: None,
        created_at: now,
        updated_at: now,
        terminal_result: None,
        final_response: None,
        error_code: None,
        stop_cause: None,
        aggregates: RunAggregates::default(),
        progress: None,
        execution: None,
        approval: None,
    };
    let store = host.ensure_orchestration_store().unwrap();
    let reservation =
        QuotaReservation::for_run(&run, DESKTOP_OWNER_ID, QuotaLimits::default(), now).unwrap();
    store
        .save_run_and_activate_agent_with_quota(&run, &agent.agent_id, &reservation)
        .unwrap();
    host.reserve_orchestration_turn(&run.run_id, lane_id)
        .unwrap();
    home.set("XAI_API_BASE", "http://127.0.0.1:1/v1");
    host.session_prompt_reserved_with_max_rounds_for_run(
        lane_id,
        "use the frozen route".into(),
        Some(1),
        &run.run_id,
        &run.run_id,
        RunExecutionMode::Shared,
    )
    .await
    .unwrap();
    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(store.list_quota_reservations().unwrap().len(), 1);
    assert_eq!(
        store
            .load_quota_reservation("quota-frozen-external")
            .unwrap()
            .unwrap()
            .reservation_id,
        "quota-frozen-external"
    );
    host.stop().unwrap();
    server.abort();
}
