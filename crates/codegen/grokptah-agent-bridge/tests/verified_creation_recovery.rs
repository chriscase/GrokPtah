//! Real start_verified_change derived receipts, before external dispatch.
//! The existing drive hold scopes this proof to creation/materialization and
//! authorization. It does not replace readiness or qualify provider/source effects.
mod common;

use grokptah_agent_bridge::orchestration::{
    canonical_workspace, ManagedExecutionBudgetProfile, ManagedGrokExecutorConfig, OrchStore,
    OrchestrationConfig, OrchestrationService, RunBounds, VerifiedChangeRequest,
    WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    directory_digest, file_digest, read_check_authority, set_grokptah_home_override,
    start_control_server, AgentHost, CredentialLeaseHandle, CredentialLeaseResolver,
    GrokBuildAdapterError, HostConfig, HostLeaseAuthority, McpControlClient, SessionKind,
    SKIP_VERIFIED_DRIVE,
};
use grokptah_agent_sdk::GrokBuildGitIdentity;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{atomic::Ordering, Arc};
use uuid::Uuid;

const TOKEN: &str = "synthetic-verified-composition-v2";
const REQUEST: &str = "original-verified-composition";
struct FileLeaseResolver(HostLeaseAuthority);
impl CredentialLeaseResolver for FileLeaseResolver {
    fn resolve(&self, id: &str) -> Result<CredentialLeaseHandle, GrokBuildAdapterError> {
        self.0.resolve(id)
    }
    fn revoke(&self, id: &str) -> Result<(), GrokBuildAdapterError> {
        self.0.revoke(id)
    }
    fn revokes_upstream(&self) -> bool {
        true
    }
}
fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: impl AsRef<Path>, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut result: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|e| e == "json"))
        .map(|e| e.into_path())
        .collect();
    result.sort();
    result
}
fn receipt(root: &Path, suffix: &str) -> Value {
    paths(&root.join("home/ledger/idempotency"))
        .into_iter()
        .map(read)
        .find(|v| v["requestId"] == format!("{REQUEST}:{suffix}"))
        .unwrap()
}
fn inventory(root: &Path) -> BTreeMap<String, Vec<u8>> {
    [
        "manager-plans",
        "work-items",
        "work-decisions",
        "work-attempts",
        "managed-intents",
        "runs",
        "idempotency",
    ]
    .into_iter()
    .flat_map(|name| paths(&root.join("home/ledger").join(name)))
    .map(|p| {
        (
            p.strip_prefix(root).unwrap().display().to_string(),
            fs::read(p).unwrap(),
        )
    })
    .collect()
}
fn child(root: &Path, phase: &str, stage: &str) -> Output {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "verified_composition_child", "--nocapture"])
        .env("VERIFIED_COMPOSITION_ROOT", root)
        .env("VERIFIED_COMPOSITION_PHASE", phase)
        .env("VERIFIED_COMPOSITION_STAGE", stage)
        .env_remove("GROKPTAH_CRASH_STAGE")
        .output()
        .unwrap()
}
fn exited(output: &Output, code: i32) {
    assert_eq!(
        output.status.code(),
        Some(code),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("/usr/bin/git")
        .current_dir(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}
fn install_profile(root: &Path, revision: u64) {
    let oracle = root.join("oracle");
    let executable = oracle.join("check.sh");
    let dir = root.join("home/ledger/check-profiles");
    fs::create_dir_all(&dir).unwrap();
    write(
        dir.join("composition.json"),
        &json!({"profileId":"composition","profileRevision":revision,
        "executable":executable,"executableDigest":file_digest(&executable).unwrap(),"args":[],
        "cwd":"oracle","oracleRoot":oracle,"oracleDigest":directory_digest(&oracle).unwrap(),
        "env":[],"timeoutMs":2000,"maxOutputBytes":1024,"network":"none","maxOutputDirBytes":65536}),
    );
}
#[test]
fn verified_composition_child() {
    let Ok(root) = std::env::var("VERIFIED_COMPOSITION_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut env = common::ProcessEnvGuard::new();
    env.set("GROKPTAH_AGENT_OFFLINE", "1");
    set_grokptah_home_override(Some(root.join("home")));
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_child(root, &mut env));
}
async fn run_child(root: PathBuf, env: &mut common::ProcessEnvGuard) {
    let phase = std::env::var("VERIFIED_COMPOSITION_PHASE").unwrap();
    let stage = std::env::var("VERIFIED_COMPOSITION_STAGE").unwrap();
    let setup = !root.join("fixture.json").exists();
    if setup {
        fs::create_dir_all(root.join("workspace/src")).unwrap();
        fs::write(
            root.join("workspace/src/fixture.rs"),
            "pub const FIXTURE: u8 = 1;\n",
        )
        .unwrap();
        git(&root.join("workspace"), &["init", "-b", "topic"]);
        git(
            &root.join("workspace"),
            &["config", "user.name", "GrokPtah test"],
        );
        git(
            &root.join("workspace"),
            &["config", "user.email", "test@grokptah.invalid"],
        );
        git(&root.join("workspace"), &["add", "src/fixture.rs"]);
        git(
            &root.join("workspace"),
            &["commit", "-m", "synthetic composition"],
        );
        for dir in ["isolate", "oracle", "fake"] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        fs::set_permissions(root.join("isolate"), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.join("fake/grok"), FAKE_GROK).unwrap();
        fs::set_permissions(root.join("fake/grok"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(root.join("fake/lease.json"), b"opaque-test-credential\n").unwrap();
        fs::set_permissions(
            root.join("fake/lease.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        fs::write(root.join("oracle/check.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            root.join("oracle/check.sh"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        install_profile(&root, 1);
    }
    let workspace = canonical_workspace(&root.join("workspace")).unwrap();
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    })
    .unwrap();
    host.start().unwrap();
    let session = if setup {
        host.session_new_kind(SessionKind::Build).unwrap().id
    } else {
        Uuid::parse_str(
            read(root.join("fixture.json"))["sessionId"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    };
    host.session_inspect(session).unwrap();
    host.session_set_cwd(session, &workspace).unwrap();
    let store = OrchStore::open(root.join("home/ledger")).unwrap();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        store.clone(),
        OrchestrationConfig {
            bearer_token: TOKEN.into(),
            allowlist: WorkspaceAllowlist::new([workspace.clone()]),
            max_concurrent_runs: 1,
            bounds: RunBounds::default(),
        },
    );
    assert!(orch.stop_background_tasks().await.fully_stopped);
    let agent = host.ensure_session_agent(session).unwrap();
    if setup {
        write(
            root.join("fixture.json"),
            &json!({"sessionId":session,"agentId":agent.agent_id,
        "head":git(&workspace, &["rev-parse","HEAD"])}),
        );
    }
    let fixture = read(root.join("fixture.json"));
    assert_eq!(fixture["agentId"], agent.agent_id);
    let head = fixture["head"].as_str().unwrap().to_string();
    orch.configure_managed_grok_executor(
        ManagedGrokExecutorConfig {
            executable: root.join("fake/grok"),
            git_executable: PathBuf::from("/usr/bin/git"),
            cwd: workspace.clone(),
            isolate_parent: root.join("isolate"),
            repository_id: "repo-composition".into(),
            base_ref: "refs/heads/topic".into(),
            identity: GrokBuildGitIdentity {
                repository_id: "repo-composition".into(),
                git_ref: "refs/heads/topic".into(),
                base_sha: head.clone(),
                head_sha: head.clone(),
            },
            credential_lease_id: "lease".into(),
        },
        Arc::new(FileLeaseResolver(HostLeaseAuthority::new(
            root.join("fake"),
        ))),
    )
    .unwrap();
    let auth = orch.auth_header(Some(&format!("Bearer {TOKEN}"))).unwrap();
    let mut request = VerifiedChangeRequest {
        request_id: REQUEST.into(),
        session_id: session,
        workspace: workspace.clone(),
        agent_id: agent.agent_id.clone(),
        objective: "inspect original fixture".into(),
        allowed_files: vec!["src/fixture.rs".into()],
        check_profile_id: "composition".into(),
        required_checks: Vec::new(),
        oracle_root: root.join("oracle"),
        budget_profile: ManagedExecutionBudgetProfile::Economy,
        mutation_mode: "isolated_review".into(),
        platform: "macos".into(),
        execution_host: "service".into(),
    };
    let prepared = orch.prepare_verified_change(&auth, &request).unwrap();
    assert_eq!(prepared["readiness"]["ready"], true, "{prepared}");
    assert_eq!(prepared["readiness"]["workersDispatched"], 0);
    assert_eq!(prepared["readiness"]["providerInvocations"], 0);
    SKIP_VERIFIED_DRIVE.store(true, Ordering::SeqCst);
    if phase == "cut" {
        env.set("GROKPTAH_CRASH_STAGE", &stage);
    }
    if phase == "changed" {
        request.objective.push_str(" changed");
    }
    if phase == "profile" {
        install_profile(&root, 2);
    }
    if matches!(phase.as_str(), "changed" | "profile" | "failed") {
        let before = inventory(&root);
        let error = orch
            .start_verified_change(&auth, &request)
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("different payload") || phase == "failed",
            "{error}"
        );
        assert_eq!(inventory(&root), before);
    } else {
        let started = orch.start_verified_change(&auth, &request).await.unwrap();
        assert_ne!(phase, "cut", "cut was not reached: {stage}");
        let work_id = started["workId"].as_str().unwrap();
        let plan_receipt = receipt(&root, "plan");
        let advance_receipt = receipt(&root, "advance");
        let authorized = receipt(&root, "authorize");
        for r in [&plan_receipt, &advance_receipt, &authorized] {
            assert_eq!(r["status"], "complete");
            assert_eq!(r["sessionId"], json!(session));
            assert_eq!(r["ownerId"], auth.owner_id);
            assert!(r["workspaceDigest"].as_str().is_some_and(|d| !d.is_empty()));
            assert!(r["error"].is_null());
        }
        assert_eq!(
            advance_receipt["response"]["createdWork"][0]["workId"],
            work_id
        );
        assert_eq!(authorized["runId"], work_id);
        let plan_id = plan_receipt["response"]["plan"]["planId"].as_str().unwrap();
        assert_eq!(advance_receipt["response"]["plan"]["planId"], plan_id);
        let plans = store.list_manager_plans().unwrap();
        let work = store.list_work_items().unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(work.len(), 2);
        assert_eq!(plans[0].plan_id, plan_id);
        assert_eq!(store.list_work_decisions(work_id).unwrap().len(), 1);
        assert!(store.list_work_attempts(None).unwrap().is_empty());
        assert!(store.list_runs().unwrap().is_empty());
        assert!(store.list_managed_intents().unwrap().is_empty());
        assert!(paths(&store.root().join("manager-intents")).is_empty());
        assert_eq!(
            fs::read_to_string(workspace.join("src/fixture.rs")).unwrap(),
            "pub const FIXTURE: u8 = 1;\n"
        );
        assert_eq!(git(&workspace, &["rev-parse", "HEAD"]), head);
        assert!(git(&workspace, &["status", "--porcelain"]).is_empty());
        let argv = fs::read_to_string(root.join("fake/grok.argv")).unwrap();
        assert!(
            argv.lines().all(|line| line == "inspect --json"),
            "worker launched: {argv}"
        );
        let private = store.verified_change_private_dir(work_id).unwrap();
        let authority = read_check_authority(&private).unwrap();
        assert_eq!(authority.work_id, work_id);
        assert_eq!(authority.profile_revision, 1);
        let envelope = read(private.join("execution-envelope.json"));
        assert_eq!(envelope["workId"], work_id);
        assert_eq!(envelope["checkAuthorityDigest"], authority.authority_digest);
        let outcomes = json!({"plan":plan_receipt,"advance":advance_receipt,"authorize":authorized,"workId":work_id});
        if root.join("outcomes.json").exists() {
            assert_eq!(read(root.join("outcomes.json")), outcomes);
        } else {
            write(root.join("outcomes.json"), &outcomes);
        }
        let expected = read(root.join("expected.json"));
        for key in ["plan", "advance", "authorize"] {
            if !expected[key].is_null() {
                assert_eq!(outcomes[key]["response"], expected[key]);
            }
        }
        if phase == "replay" {
            let server = start_control_server(orch.clone(), 0).await.unwrap();
            let mut client = McpControlClient::new(format!("http://{}", server.addr), TOKEN);
            client.initialize().await.unwrap();
            let expected_ids: BTreeSet<_> = work.iter().map(|w| w.work_id.clone()).collect();
            for graph in [false, true] {
                let mut cursor = None;
                let mut ids = Vec::new();
                loop {
                    let mut input = json!({"session_id":session,"workspace":workspace,"limit":1});
                    if let Some(cursor) = &cursor {
                        input["cursor"] = json!(cursor);
                    }
                    let page = client
                        .call_tool(
                            if graph {
                                "ptah_get_work_graph_page"
                            } else {
                                "ptah_list_work_page"
                            },
                            input,
                        )
                        .await
                        .unwrap();
                    assert!(!page.is_error, "{page:?}");
                    let body = page.structured;
                    assert!(serde_json::to_vec(&body).unwrap().len() < 32768);
                    for item in body[if graph { "nodes" } else { "items" }]
                        .as_array()
                        .unwrap()
                    {
                        ids.push(item["workId"].as_str().unwrap().to_string());
                    }
                    if body["complete"] == true {
                        break;
                    }
                    cursor = Some(body["continuation"].as_str().unwrap().to_string());
                    assert!(ids.len() <= 2);
                }
                assert_eq!(ids.len(), 2);
                assert_eq!(ids.into_iter().collect::<BTreeSet<_>>(), expected_ids);
            }
            let detail = client
                .call_tool(
                    "ptah_get_work_detail",
                    json!({"session_id":session,"workspace":workspace,"work_id":work_id,"section":"summary"}),
                )
                .await
                .unwrap();
            assert!(!detail.is_error, "{detail:?}");
            assert_eq!(detail.structured["body"]["workId"], work_id);
            assert!(server.stop_and_wait().await.fully_stopped);
        }
        if phase != "replay" {
            assert_eq!(read(root.join("outcomes.json")), outcomes);
        }
    }
    SKIP_VERIFIED_DRIVE.store(false, Ordering::SeqCst);
    let _ = host.shutdown().await;
}
fn seal_expected(root: &Path) {
    let mut expected = json!({});
    for suffix in ["plan", "advance", "authorize"] {
        for p in paths(&root.join("home/ledger/idempotency")) {
            let r = read(p);
            if r["requestId"] == format!("{REQUEST}:{suffix}") && r["status"] == "complete" {
                expected[suffix] = r["response"].clone();
            }
        }
    }
    for path in paths(&root.join("home/ledger/manager-intents")) {
        let i = read(path);
        if i["schemaVersion"] == 2 {
            expected["plan"] = json!({"plan":i["plan"],"rootWork":i["rootWork"]});
        } else {
            expected["advance"] = json!({"plan":i["plan"],"createdWork":i["workWrites"].as_array().unwrap().iter().map(|w|w["item"].clone()).collect::<Vec<_>>()});
        }
    }
    for path in paths(&root.join("home/ledger/work-intents")) {
        let i = read(path);
        expected["authorize"] = match i["request"]["shape"].as_str().unwrap() {
            "decision" => json!({"work":i["item"],"decision":i["decision"]}),
            "decision_scope" => {
                json!({"work":i["item"],"decision":i["decision"],"sessionId":i["request"]["sessionId"],"workspace":i["request"]["workspaceDisplay"]})
            }
            other => panic!("unexpected authorization shape {other}"),
        };
    }
    write(root.join("expected.json"), &expected);
}
fn prove(stage: &str) {
    let root = tempfile::tempdir().unwrap();
    exited(&child(root.path(), "cut", stage), 86);
    seal_expected(root.path());
    for phase in ["replay", "reopen-a", "reopen-b"] {
        let before = inventory(root.path());
        exited(&child(root.path(), phase, ""), 0);
        if phase != "replay" {
            assert_eq!(inventory(root.path()), before);
        }
    }
    exited(&child(root.path(), "changed", ""), 0);
}
#[test]
fn derived_manager_creation_and_materialization_replay_original_receipts() {
    for stage in [
        "manager_create_after_intent",
        "manager_create_after_work",
        "manager_create_after_plan",
        "manager_create_after_receipt",
        "manager_create_after_intent_remove",
        "manager_create_after_commit",
        "manager_mutation_after_intent",
        "manager_mutation_after_first_work",
        "manager_mutation_after_work",
        "manager_mutation_after_plan",
        "manager_mutation_after_receipt",
        "manager_mutation_after_commit",
    ] {
        prove(stage);
    }
}
#[test]
fn derived_authorization_replays_without_launching_or_minting_attempts() {
    for stage in [
        "mutation_after_intent",
        "mutation_after_decision",
        "mutation_after_item",
        "mutation_after_receipt",
        "mutation_after_intent_remove",
    ] {
        prove(stage);
    }
}
#[test]
fn derived_pre_admission_cuts_do_not_create_successors() {
    for (stage, work_count) in [
        ("manager_create_before_intent", 0),
        ("manager_mutation_before_intent", 1),
        ("mutation_before_intent", 2),
    ] {
        let root = tempfile::tempdir().unwrap();
        exited(&child(root.path(), "cut", stage), 86);
        assert!(paths(&root.path().join("home/ledger/manager-intents")).is_empty());
        assert_eq!(
            paths(&root.path().join("home/ledger/work-items")).len(),
            work_count
        );
        exited(&child(root.path(), "failed", ""), 0);
    }
}
#[test]
fn changed_profile_after_a_creation_cut_refuses_without_new_work() {
    let root = tempfile::tempdir().unwrap();
    exited(&child(root.path(), "cut", "manager_create_after_work"), 86);
    exited(&child(root.path(), "profile", ""), 0);
    assert_eq!(paths(&root.path().join("home/ledger/work-items")).len(), 1);
    assert!(paths(&root.path().join("home/ledger/work-attempts")).is_empty());
}

const FAKE_GROK: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$(dirname "$0")/grok.argv"
if [ "$1" = "inspect" ] && [ "$2" = "--json" ]; then
  printf '{"grokVersion":"1.0.5","channel":"stable","cwd":"%s","projectRoot":"%s","projectTrusted":true,"projectInstructions":[],"permissions":{"loaded":0,"managedSettingsActive":false,"managedSettingsExists":false,"managedSettingsPath":"/managed/settings","marketplaceAllowlist":[],"mcpServerAllowlist":[],"skipped":[],"sources":[]},"loginPolicy":{"apiKeyAuthDisabled":false,"disableApiKeyAuth":null,"forceLoginTeamUuid":null},"hooks":[],"skills":[],"agents":[],"plugins":[],"marketplaces":[],"mcpServers":[],"lspServers":[],"configSources":{"layers":[{"path":"%s/config.toml","role":"user"}]},"externalCompat":{"cells":[{"enabled":false,"source":"config","surface":"skills","vendor":"cursor"},{"enabled":false,"source":"config","surface":"rules","vendor":"cursor"},{"enabled":false,"source":"config","surface":"agents","vendor":"cursor"},{"enabled":false,"source":"config","surface":"mcps","vendor":"cursor"},{"enabled":false,"source":"config","surface":"hooks","vendor":"cursor"},{"enabled":false,"source":"config","surface":"sessions","vendor":"cursor"},{"enabled":false,"source":"config","surface":"skills","vendor":"claude"},{"enabled":false,"source":"config","surface":"rules","vendor":"claude"},{"enabled":false,"source":"config","surface":"agents","vendor":"claude"},{"enabled":false,"source":"config","surface":"mcps","vendor":"claude"},{"enabled":false,"source":"config","surface":"hooks","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"claude"},{"enabled":false,"source":"config","surface":"sessions","vendor":"codex"}],"remoteSettingsLoaded":false}}\n' "$PWD" "$PWD" "$GROK_HOME"
  exit 0
fi
exit 73
"#;
