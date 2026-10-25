//! Real create callers share idempotency and capacity authority under races.
mod common;

use std::path::Path;
use std::sync::{Arc, Barrier};

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, safe_id_filename, AuthContext, IdempotencyScope, OrchError, OrchErrorCode,
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkItem, WorkPolicy,
    WorkspaceAllowlist, MAX_GRAPH_SCOPE_ITEMS,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, SessionKind};
use serde_json::Value;
use uuid::Uuid;

const TOKEN: &str = "synthetic-creation-conformance";

struct GateReset;

impl Drop for GateReset {
    fn drop(&mut self) {
        OrchStore::set_work_request_test_gate(None);
    }
}

async fn create(
    orch: &OrchestrationService,
    auth: &AuthContext,
    session: Uuid,
    workspace: &Path,
    request: &str,
    objective: &str,
) -> Result<Value, OrchError> {
    orch.create_work(
        auth,
        request,
        session,
        workspace,
        "test".into(),
        objective.into(),
        0,
        None,
        None,
        Vec::new(),
        WorkPolicy::default(),
    )
    .await
}

fn race(
    orch: &Arc<OrchestrationService>,
    auth: &AuthContext,
    lanes: [(Uuid, &Path); 2],
    requests: [&str; 2],
) -> Vec<Result<Value, OrchError>> {
    // Different requests rendezvous after operation-local binding installation
    // and before store adoption. Duplicates only rendezvous before claiming: the
    // duplicate must replay and must never reach a second store admission.
    let _reset = GateReset;
    if requests[0] != requests[1] {
        OrchStore::set_work_request_test_gate(Some(Arc::new(Barrier::new(2))));
    }
    let start = Arc::new(Barrier::new(2));
    let workers = (0..2)
        .map(|index| {
            let orch = orch.clone();
            let auth = auth.clone();
            let session = lanes[index].0;
            let workspace = lanes[index].1.to_path_buf();
            let request = requests[index].to_string();
            let start = start.clone();
            std::thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                start.wait();
                runtime.block_on(create(
                    &orch, &auth, session, &workspace, &request, &request,
                ))
            })
        })
        .collect::<Vec<_>>();
    workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn creation_races_and_refusals_preserve_request_scope_and_original_outcomes() {
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(root.path().join("home")));
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let first = canonical_workspace(&first).unwrap();
    let second = canonical_workspace(&second).unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let a = host.session_new_kind(SessionKind::Build).unwrap().id;
    let b = host.session_new_kind(SessionKind::Build).unwrap().id;
    host.session_set_cwd(a, &first).unwrap();
    host.session_set_cwd(b, &second).unwrap();
    let store = OrchStore::open(root.path().join("ledger")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([first.clone(), second.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();

    let duplicate = race(
        &orch,
        &auth,
        [(a, &first), (a, &first)],
        ["duplicate", "duplicate"],
    );
    let original = duplicate[0].as_ref().unwrap();
    assert_eq!(duplicate[1].as_ref().unwrap(), original);
    assert_eq!(store.list_work_items().unwrap().len(), 1);
    let changed = create(&orch, &auth, a, &first, "duplicate", "different payload")
        .await
        .unwrap_err();
    assert_eq!(changed.code, OrchErrorCode::Conflict);
    assert_eq!(
        create(&orch, &auth, a, &first, "duplicate", "duplicate")
            .await
            .unwrap(),
        *original
    );

    let same = race(
        &orch,
        &auth,
        [(a, &first), (a, &first)],
        ["same-a", "same-b"],
    );
    assert_ne!(
        same[0].as_ref().unwrap()["work"]["workId"],
        same[1].as_ref().unwrap()["work"]["workId"]
    );
    let different = race(
        &orch,
        &auth,
        [(a, &first), (b, &second)],
        ["lane-a", "lane-b"],
    );
    for (result, session, workspace, request) in [
        (&same[0], a, &first, "same-a"),
        (&same[1], a, &first, "same-b"),
        (&different[0], a, &first, "lane-a"),
        (&different[1], b, &second, "lane-b"),
    ] {
        let response = result.as_ref().unwrap();
        assert_eq!(response["work"]["objective"], request);
        assert_eq!(response["work"]["sessionId"], session.to_string());
        assert_eq!(
            response["work"]["workspace"],
            workspace.display().to_string()
        );
        let scope = IdempotencyScope::new(&auth.owner_id, session, workspace).unwrap();
        let receipt = store.load_idempotency(&scope, request).unwrap().unwrap();
        assert_eq!(receipt.status, "complete");
        assert_eq!(receipt.response, *response);
        assert_eq!(
            receipt.run_id.as_deref(),
            response["work"]["workId"].as_str()
        );
        assert_eq!(
            create(&orch, &auth, session, workspace, request, request)
                .await
                .unwrap(),
            *response
        );
    }
    // The service must authorize the session/workspace pair before receipt lookup.
    let refused = create(&orch, &auth, a, &second, "duplicate", "duplicate")
        .await
        .unwrap_err();
    assert_eq!(refused.code, OrchErrorCode::WorkspaceMismatch);
    assert_eq!(store.list_work_items().unwrap().len(), 5);

    // Canonical authority owns the stored lane even if the caller uses an alias.
    let alias = first.join(".");
    let aliased = create(&orch, &auth, a, &alias, "alias", "alias")
        .await
        .unwrap();
    assert_eq!(aliased["work"]["workspace"], first.display().to_string());
    assert_eq!(
        create(&orch, &auth, a, &alias, "alias", "alias")
            .await
            .unwrap(),
        aliased
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let symlink_lane = root.path().join("symlink-lane");
        symlink(&first, &symlink_lane).unwrap();
        let symlinked = create(&orch, &auth, a, &symlink_lane, "symlink-alias", "symlink")
            .await
            .unwrap();
        assert_eq!(symlinked["work"]["workspace"], first.display().to_string());
        assert_eq!(
            create(&orch, &auth, a, &symlink_lane, "symlink-alias", "symlink")
                .await
                .unwrap(),
            symlinked
        );
    }
    assert_eq!(
        store.list_work_items().unwrap().len(),
        if cfg!(unix) { 7 } else { 6 }
    );
    assert!(store.list_work_attempts(None).unwrap().is_empty());
    let stopped = orch.stop_background_tasks().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    drop(orch);
    assert!(host.shutdown().await.is_clean());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn competing_creates_serialize_the_last_capacity_slot_and_replay_both_outcomes() {
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    let root = tempfile::tempdir().unwrap();
    set_grokptah_home_override(Some(root.path().join("home")));
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let workspace = canonical_workspace(&root.path().join("workspace")).unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap().id;
    host.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.path().join("ledger")).unwrap();
    for index in 0..MAX_GRAPH_SCOPE_ITEMS - 1 {
        let mut item = WorkItem::new(
            "test",
            "capacity fixture",
            session,
            workspace.display().to_string(),
            "synthetic-fixture",
            WorkPolicy::default(),
        )
        .unwrap();
        item.work_id = format!("seed-{index:04}");
        item.validate().unwrap();
        std::fs::write(
            store
                .root()
                .join("work-items")
                .join(format!("{}.json", safe_id_filename(&item.work_id).unwrap())),
            serde_json::to_vec_pretty(&item).unwrap(),
        )
        .unwrap();
    }
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 2,
            bounds: RunBounds::default(),
        },
    );
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let results = race(
        &orch,
        &auth,
        [(session, &workspace), (session, &workspace)],
        ["last-a", "last-b"],
    );
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    assert_eq!(
        store.list_work_items().unwrap().len(),
        MAX_GRAPH_SCOPE_ITEMS
    );
    for (result, request) in results.iter().zip(["last-a", "last-b"]) {
        let replay = create(&orch, &auth, session, &workspace, request, request).await;
        match result {
            Ok(response) => assert_eq!(replay.unwrap(), *response),
            Err(error) => {
                assert_eq!(error.code, OrchErrorCode::CapacityExhausted);
                assert_eq!(
                    serde_json::to_value(replay.unwrap_err()).unwrap(),
                    serde_json::to_value(error).unwrap()
                );
            }
        }
    }
    assert!(store.ledger_admission_closed().is_none());
    assert_eq!(
        std::fs::read_dir(store.root().join("work-lifecycle-intents"))
            .unwrap()
            .count(),
        0
    );
    let stopped = orch.stop_background_tasks().await;
    assert!(stopped.fully_stopped && stopped.errors.is_empty());
    drop(orch);
    assert!(host.shutdown().await.is_clean());
}
