//! Opt-in live GrokPtah-on-GrokPtah smoke. Requires GROKPTAH_LIVE_SELF_HOST=1
//! and a usable Grok Build route. Never prints credentials.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use grokptah_agent_bridge::account_facts::grok_account_facts;
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunExecutionMode, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{set_grokptah_home_override, AgentHost, HostConfig, SessionKind};
use grokptah_agent_sdk::account::CredentialMethod;
use serde_json::json;
use tempfile::tempdir;

fn live_enabled() -> bool {
    std::env::var("GROKPTAH_LIVE_SELF_HOST").ok().as_deref() == Some("1")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::await_holding_lock)]
async fn supervised_grokptah_on_grokptah_live_smoke() {
    if !live_enabled() {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    let facts = grok_account_facts(now);
    assert!(
        matches!(
            facts.credential_method,
            CredentialMethod::GrokBuildOidc | CredentialMethod::GrokBuildApiKey
        ),
        "live smoke requires a Grok Build route, got {}",
        facts.credential_method.as_str()
    );
    assert!(facts.permits_launch(), "Grok Build route is not launchable");

    let home = tempdir().unwrap();
    set_grokptah_home_override(Some(home.path().join(".grokptah")));

    let dogfood = PathBuf::from("/private/tmp/grokptah-self-host-dogfood-checkout");
    if dogfood.exists() {
        let _ = std::fs::remove_dir_all(&dogfood);
    }
    let src = repo_root().canonicalize().unwrap();
    let cloned = git(
        &[
            "clone",
            "--no-local",
            src.to_str().unwrap(),
            dogfood.to_str().unwrap(),
        ],
        Path::new("/private/tmp"),
    );
    assert!(
        cloned.status.success(),
        "clone failed: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let pin = git(&["rev-parse", "HEAD"], &src);
    let pin = String::from_utf8_lossy(&pin.stdout).trim().to_string();
    let co = git(&["checkout", "--detach", &pin], &dogfood);
    assert!(co.status.success(), "pin failed");

    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start().unwrap();
    host.set_project_cwd(&dogfood).unwrap();
    let session = host.session_new_kind(SessionKind::Build).unwrap();
    host.session_set_cwd(session.id, &dogfood).unwrap();
    host.session_set_execution_mode(session.id, RunExecutionMode::IsolatedWorktree, false)
        .expect("isolated mode");

    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(home.path().join("orch")).unwrap(),
        OrchestrationConfig {
            bearer_token: "self-host-live-token".into(),
            allowlist: WorkspaceAllowlist::new([dogfood.clone()]),
            max_concurrent_runs: 1,
            bounds: Default::default(),
        },
    );
    let auth = orch
        .auth_header(Some("Bearer self-host-live-token"))
        .unwrap();
    let prompt = "Inspect the candidate's physical-send or durable-continuity implementation. \
Add one narrowly scoped unit test for an already implemented safety invariant, without changing \
production behavior. Run only the relevant formatting and focused test. Return the exact diff, \
test result, Run/attempt/provider-request/receipt identities, and request explicit human Review. \
Do not commit, push, open a PR, promote, or access paths outside this disposable checkout.";
    let submitted = orch
        .submit_task_with_execution_mode(
            &auth,
            "self-host-live-1",
            session.id,
            &dogfood,
            prompt.into(),
            None,
            RunExecutionMode::IsolatedWorktree,
        )
        .await
        .expect("submit isolated live task");
    let run_id = submitted["runId"].as_str().expect("run id").to_string();
    eprintln!(
        "LIVE_SELF_HOST_SUBMIT run={} exec={:?} authority={}",
        run_id,
        submitted.get("executionMode"),
        submitted.get("authority").is_some()
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(240);
    let mut last = json!({});
    loop {
        last = orch
            .get_run_scoped(&auth, session.id, &dogfood, &run_id)
            .expect("get run");
        let state = last["state"].as_str().unwrap_or("");
        if matches!(
            state,
            "completed" | "failed" | "cancelled" | "interrupted" | "limit_reached"
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "live run did not finish: {last}"
        );
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
    let encoded = serde_json::to_string(&last).unwrap();
    for needle in ["Bearer ", "refresh_token", "XAI_API_KEY", "mac_key", "hmac"] {
        assert!(
            !encoded.contains(needle),
            "live run leaked {needle}: {encoded}"
        );
    }
    eprintln!(
        "LIVE_SELF_HOST_TERMINAL state={} send={} attempt={} providerRequest={} authorityGrant={}",
        last["state"],
        last["sendState"],
        last["attemptId"],
        last["providerRequestId"],
        last["authority"]["grantClass"]
    );

    if last["state"] == "completed" {
        let review = orch
            .review_run(&auth, session.id, &dogfood, &run_id)
            .expect("review");
        eprintln!(
            "LIVE_SELF_HOST_REVIEW fingerprint={} files={}",
            review["fingerprint"],
            review["changedFiles"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
        );
        let discarded = orch
            .discard_run(&auth, "self-host-discard-1", session.id, &dogfood, &run_id)
            .await
            .expect("discard");
        eprintln!("LIVE_SELF_HOST_DISCARD {}", discarded["promotionState"]);
    }

    host.stop().unwrap();
    set_grokptah_home_override(None);
}
