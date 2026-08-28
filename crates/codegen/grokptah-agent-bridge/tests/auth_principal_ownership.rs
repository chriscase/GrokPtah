//! Adversarial principal-ownership tests for orchestration run reads.
//!
//! Two named credentials share one service, one session and one workspace, so
//! every denial here is decided by principal ownership alone — session and
//! workspace scope are identical on both sides and cannot be what refuses the
//! read. The refusal for another principal's run must be byte-identical to the
//! refusal for a run id that does not exist, or the read paths become an
//! existence oracle.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Utc;
use grokptah_agent_bridge::orchestration::{
    AuthContext, AuthCredential, OrchError, OrchErrorCode, OrchStore, OrchestrationConfig,
    OrchestrationService, RunBounds, RunRecord, RunState, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, AgentHostHandle, HostConfig};
use tempfile::TempDir;
use uuid::Uuid;

use common::ProcessEnvGuard;

const TOKEN_PRIMARY: &str = "ownership-token-primary";
const TOKEN_DEVICE: &str = "ownership-token-device";

/// Runs seeded for the matrix, by the principal stamped on them.
const RUN_MCP: &str = "run-owned-by-mcp";
const RUN_DEVICE: &str = "run-owned-by-device";
const RUN_NATIVE: &str = "run-owned-by-native-executor";
const RUN_DESKTOP: &str = "run-owned-by-desktop";
const RUN_LEGACY: &str = "run-with-no-principal";
const RUN_ABSENT: &str = "run-that-does-not-exist";

struct Harness {
    _home: TempDir,
    _guard: ProcessEnvGuard,
    workspace: TempDir,
    host: AgentHostHandle,
    orch: Arc<OrchestrationService>,
    session_id: Uuid,
    /// Canonical workspace string exactly as the service records it.
    claimed: String,
}

impl Harness {
    fn mcp(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN_PRIMARY}")))
            .unwrap()
    }

    fn device(&self) -> AuthContext {
        self.orch
            .auth_header(Some(&format!("Bearer {TOKEN_DEVICE}")))
            .unwrap()
    }

    fn workspace_path(&self) -> PathBuf {
        self.workspace.path().to_path_buf()
    }

    fn seed_run(&self, run_id: &str, client_id: Option<&str>) {
        let run = RunRecord {
            run_id: run_id.into(),
            session_id: self.session_id,
            workspace: self.claimed.clone(),
            request_id: format!("req-{run_id}"),
            client_id: client_id.map(str::to_string),
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
            prompt_preview: "seeded".into(),
            start_seq: Some(1),
            end_seq: Some(2),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            terminal_result: Some("done".into()),
            final_response: None,
            error_code: None,
            stop_cause: None,
            aggregates: Default::default(),
            progress: None,
            execution: None,
            approval: None,
        };
        self.orch.store().save_run(&run).unwrap();
    }

    fn stored(&self, run_id: &str) -> Option<String> {
        self.orch
            .store()
            .load_run(run_id)
            .unwrap()
            .map(|run| serde_json::to_string(&run).unwrap())
    }
}

fn harness() -> Harness {
    let mut guard = ProcessEnvGuard::new();
    let home = tempfile::tempdir().unwrap();
    let home_dir = home.path().join(".grokptah");
    std::fs::create_dir_all(&home_dir).unwrap();
    set_grokptah_home_override(Some(home_dir));
    guard.set("GROKPTAH_AGENT_OFFLINE", "1");

    let workspace = tempfile::tempdir().unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().expect("host starts");
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: TOKEN_PRIMARY.into(),
            allowlist: WorkspaceAllowlist::new([workspace.path().to_path_buf()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    // Two credentials on one service: the compatibility primary (wire principal
    // "mcp") and a named device credential (principal "device-b").
    orch.set_auth_credentials(vec![
        AuthCredential::new("primary", TOKEN_PRIMARY).unwrap(),
        AuthCredential::new("device-b", TOKEN_DEVICE).unwrap(),
    ])
    .unwrap();

    let auth = orch
        .auth_header(Some(&format!("Bearer {TOKEN_PRIMARY}")))
        .unwrap();
    let session = orch.create_session(&auth, workspace.path(), None).unwrap();
    let session_id = session["sessionId"].as_str().unwrap().parse().unwrap();
    let claimed = session["workspace"].as_str().unwrap().to_string();

    let h = Harness {
        _home: home,
        _guard: guard,
        workspace,
        host,
        orch,
        session_id,
        claimed,
    };
    h.seed_run(RUN_MCP, Some("mcp"));
    h.seed_run(RUN_DEVICE, Some("device-b"));
    h.seed_run(RUN_NATIVE, Some("native-executor"));
    h.seed_run(RUN_DESKTOP, Some("desktop"));
    h.seed_run(RUN_LEGACY, None);
    h
}

/// One run-scoped read, invoked uniformly whether or not it takes a session
/// and workspace, so the whole read surface can be asserted in one matrix.
type ReadPath = fn(
    &OrchestrationService,
    &AuthContext,
    Uuid,
    &Path,
    &str,
) -> Result<serde_json::Value, OrchError>;

/// Every run-scoped read, addressed both unscoped and session/workspace-scoped.
fn read_paths() -> Vec<(&'static str, ReadPath)> {
    vec![
        ("get_run", |o, a, _s, _w, r| o.get_run(a, r)),
        ("get_run_scoped", |o, a, s, w, r| {
            o.get_run_scoped(a, s, w, r)
        }),
        ("get_progress", |o, a, _s, _w, r| o.get_progress(a, r)),
        ("get_progress_scoped", |o, a, s, w, r| {
            o.get_progress_scoped(a, s, w, r)
        }),
        ("get_events", |o, a, _s, _w, r| {
            o.get_events(a, Some(r), 0, 10)
        }),
        ("get_events_scoped", |o, a, s, w, r| {
            o.get_events_scoped(a, s, w, r, 0, 10)
        }),
        ("get_changes", |o, a, _s, _w, r| o.get_changes(a, r)),
        ("get_changes_scoped", |o, a, s, w, r| {
            o.get_changes_scoped(a, s, w, r)
        }),
        ("get_test_results", |o, a, _s, _w, r| {
            o.get_test_results(a, r)
        }),
        ("get_test_results_scoped", |o, a, s, w, r| {
            o.get_test_results_scoped(a, s, w, r)
        }),
        ("get_handoff", |o, a, _s, _w, r| o.get_handoff(a, r)),
        ("get_handoff_scoped", |o, a, s, w, r| {
            o.get_handoff_scoped(a, s, w, r)
        }),
    ]
}

fn denial_text(error: &OrchError) -> String {
    format!("{}|{}", error.code.as_str(), error.message)
}

#[tokio::test]
async fn foreign_run_reads_are_byte_identical_to_unknown_run_reads() {
    let h = harness();
    let ws = h.workspace_path();

    for (name, call) in read_paths() {
        let mcp = h.mcp();
        // The caller's own run is readable through this path.
        call(&h.orch, &mcp, h.session_id, &ws, RUN_MCP)
            .unwrap_or_else(|e| panic!("{name} must serve the caller's own run: {e:?}"));

        // Another principal's run, and a run id that does not exist, must be
        // refused identically — same code, same message, same bytes.
        let foreign = call(&h.orch, &mcp, h.session_id, &ws, RUN_DEVICE)
            .expect_err(&format!("{name} must refuse another principal's run"));
        let absent = call(&h.orch, &mcp, h.session_id, &ws, RUN_ABSENT)
            .expect_err(&format!("{name} must refuse an unknown run"));
        assert_eq!(
            denial_text(&foreign),
            denial_text(&absent),
            "{name}: foreign and unknown denials must be indistinguishable"
        );
        assert_eq!(foreign.code, OrchErrorCode::InvalidRequest, "{name}");

        // Symmetric: the device credential sees the mirror image.
        let device = h.device();
        call(&h.orch, &device, h.session_id, &ws, RUN_DEVICE)
            .unwrap_or_else(|e| panic!("{name} must serve device-b its own run: {e:?}"));
        let foreign = call(&h.orch, &device, h.session_id, &ws, RUN_MCP)
            .expect_err(&format!("{name} must refuse mcp's run to device-b"));
        assert_eq!(denial_text(&foreign), denial_text(&absent), "{name}");
    }
}

#[tokio::test]
async fn unowned_and_desktop_runs_are_refused_and_native_executor_runs_are_shared() {
    let h = harness();
    let ws = h.workspace_path();
    let absent_denial = denial_text(&h.orch.get_run(&h.mcp(), RUN_ABSENT).unwrap_err());

    for (name, call) in read_paths() {
        for auth in [h.mcp(), h.device()] {
            // The one deliberate exception: the in-process managed executor
            // submits on behalf of coordinator-created work, so both
            // credentials in scope may read its runs.
            call(&h.orch, &auth, h.session_id, &ws, RUN_NATIVE)
                .unwrap_or_else(|e| panic!("{name} must serve shared native-executor runs: {e:?}"));

            // Desktop turns and principal-less legacy records belong to nobody
            // on this control plane.
            for run_id in [RUN_DESKTOP, RUN_LEGACY] {
                let error = call(&h.orch, &auth, h.session_id, &ws, run_id)
                    .expect_err(&format!("{name} must refuse {run_id}"));
                assert_eq!(denial_text(&error), absent_denial, "{name}/{run_id}");
            }
        }
    }
}

#[tokio::test]
async fn listing_never_surfaces_runs_the_caller_could_not_fetch() {
    let h = harness();
    let ws = h.workspace_path();

    for (auth, own) in [(h.mcp(), RUN_MCP), (h.device(), RUN_DEVICE)] {
        let listed = h.orch.list_runs_scoped(&auth, h.session_id, &ws).unwrap();
        let ids: Vec<String> = listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|run| run["runId"].as_str().unwrap().to_string())
            .collect();
        assert!(ids.contains(&own.to_string()), "own run must be listed");
        assert!(
            ids.contains(&RUN_NATIVE.to_string()),
            "shared native-executor runs stay listed"
        );
        for hidden in [RUN_DESKTOP, RUN_LEGACY] {
            assert!(
                !ids.contains(&hidden.to_string()),
                "{hidden} must be hidden"
            );
        }
        // Anything listed must also be individually fetchable: listing and
        // per-run reads must not disagree.
        for id in &ids {
            h.orch
                .get_run_scoped(&auth, h.session_id, &ws, id)
                .unwrap_or_else(|e| panic!("listed run {id} must be readable: {e:?}"));
        }
    }

    let mcp_ids: Vec<String> = h
        .orch
        .list_runs_scoped(&h.mcp(), h.session_id, &ws)
        .unwrap()["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|run| run["runId"].as_str().unwrap().to_string())
        .collect();
    assert!(!mcp_ids.contains(&RUN_DEVICE.to_string()));
}

#[tokio::test]
async fn follow_up_paths_refuse_foreign_runs_with_zero_side_effects() {
    let h = harness();
    let ws = h.workspace_path();
    let mcp = h.mcp();
    let before = h.stored(RUN_DEVICE).expect("seeded run exists");
    let runs_before = h.orch.store().list_runs().unwrap().len();
    let sessions_before = h.host.list_sessions().len();
    let absent_denial = denial_text(&h.orch.get_run(&mcp, RUN_ABSENT).unwrap_err());

    let review = h
        .orch
        .review_run(&mcp, h.session_id, &ws, RUN_DEVICE)
        .unwrap_err();
    assert_eq!(denial_text(&review), absent_denial, "review_run");

    let discard = h
        .orch
        .discard_run(&mcp, "req-discard", h.session_id, &ws, RUN_DEVICE)
        .await
        .unwrap_err();
    assert_eq!(denial_text(&discard), absent_denial, "discard_run");

    let promote = h
        .orch
        .promote_run(&mcp, "req-promote", h.session_id, &ws, RUN_DEVICE, "appr")
        .await
        .unwrap_err();
    assert_eq!(denial_text(&promote), absent_denial, "promote_run");

    let cancel = h
        .orch
        .cancel(&mcp, "req-cancel", h.session_id, &ws, Some(RUN_DEVICE))
        .await
        .unwrap_err();
    assert_eq!(denial_text(&cancel), absent_denial, "cancel");

    let retry = h
        .orch
        .retry_run(
            &mcp,
            "req-retry",
            h.session_id,
            &ws,
            RUN_DEVICE,
            "retry the foreign run".into(),
            None,
            None,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(denial_text(&retry), absent_denial, "retry_run");

    // Nothing moved: the foreign record is byte-identical, no run was created
    // and no session was spun up on the way to the refusal.
    assert_eq!(h.stored(RUN_DEVICE).as_deref(), Some(before.as_str()));
    assert_eq!(h.orch.store().list_runs().unwrap().len(), runs_before);
    assert_eq!(h.host.list_sessions().len(), sessions_before);
}

#[tokio::test]
async fn rotation_during_reads_is_decided_before_ownership() {
    let h = harness();
    let ws = h.workspace_path();
    let before_rotation = h.mcp();
    h.orch
        .get_run_scoped(&before_rotation, h.session_id, &ws, RUN_MCP)
        .unwrap();

    // Rotate mid-read-session.
    h.orch
        .set_auth_credentials(vec![
            AuthCredential::new("primary", TOKEN_PRIMARY).unwrap(),
            AuthCredential::new("device-b", TOKEN_DEVICE).unwrap(),
        ])
        .unwrap();

    // The stale context is refused as stale — even for a run it owns, and even
    // for a run it does not. Ownership is never consulted, so rotation cannot
    // be used to probe which runs exist.
    for run_id in [RUN_MCP, RUN_DEVICE, RUN_ABSENT] {
        let error = h
            .orch
            .get_run_scoped(&before_rotation, h.session_id, &ws, run_id)
            .unwrap_err();
        assert_eq!(
            error.code,
            OrchErrorCode::Unauthenticated,
            "{run_id}: the epoch guard must decide before ownership"
        );
    }

    // Re-authentication restores exactly the previous authority, no more.
    let after_rotation = h.mcp();
    h.orch
        .get_run_scoped(&after_rotation, h.session_id, &ws, RUN_MCP)
        .unwrap();
    assert_eq!(
        h.orch
            .get_run_scoped(&after_rotation, h.session_id, &ws, RUN_DEVICE)
            .unwrap_err()
            .code,
        OrchErrorCode::InvalidRequest
    );
}
