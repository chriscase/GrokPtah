//! Explicit, one-attempt qualification through the production MCP controls.
//! Readiness never dispatches. Execute leaves the candidate in durable review.
//! Apply/discard are separate invocations after inspection of the saved report.

use anyhow::{bail, ensure, Context, Result};
use grokptah_agent_bridge::orchestration::{
    OrchStore, OrchestrationConfig, OrchestrationService, RunBounds, WorkspaceAllowlist,
};
use grokptah_agent_bridge::{
    directory_digest, file_digest, set_grokptah_home_override, start_control_server, AgentHost,
    HostConfig, SessionKind,
};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use uuid::Uuid;

const FRAMING: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/source/src/framing.py");
const DECODER: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/source/src/decoder.py");
const ORACLE: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/oracle/check.py");
const CHECK: &str =
    include_str!("../../../../evals/fixtures/real-managed-assignment-v1/oracle/check.sh");
const OBJECTIVE: &str = "Inspect src/framing.py and src/decoder.py and repair both functions. encode(str) must emit the UTF-8 BYTE length as canonical unsigned ASCII decimal followed by a colon and the payload. decode(bytes) must accept exactly one frame, require header 0 or a nonzero decimal without leading zeros, require the exact payload byte length, and reject malformed headers, truncated/excess payloads, and invalid UTF-8 with ValueError. Preserve valid empty, ASCII, Unicode and embedded-colon messages. Modify exactly these two files. Use file tools only; do not run commands or change any oracle. Finish with a brief description of both changes and a final line GROK_BUILD_VERDICT=clean, or GROK_BUILD_VERDICT=not_complete if unfinished.";

fn git(source: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("/usr/bin/git")
        .current_dir(source)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "qualification Git operation failed"
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn save(path: &Path, value: &Value) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn initialize(root: &Path) -> Result<Value> {
    let marker = root.join("qualification.json");
    if marker.is_file() {
        return Ok(serde_json::from_slice(&fs::read(marker)?)?);
    }
    ensure!(
        fs::read_dir(root)?.next().is_none(),
        "qualification root must be empty"
    );
    let source = root.join("source");
    let oracle = root.join("oracle");
    fs::create_dir_all(source.join("src"))?;
    fs::create_dir(&oracle)?;
    fs::create_dir(root.join("worker-root"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.join("worker-root"), fs::Permissions::from_mode(0o700))?;
    }
    fs::write(source.join("src/framing.py"), FRAMING)?;
    fs::write(source.join("src/decoder.py"), DECODER)?;
    fs::write(oracle.join("check.py"), ORACLE)?;
    fs::write(oracle.join("check.sh"), CHECK)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(oracle.join("check.sh"), fs::Permissions::from_mode(0o700))?;
    }
    git(&source, &["init", "-b", "qualification"])?;
    git(&source, &["config", "user.name", "GrokPtah qualification"])?;
    git(
        &source,
        &["config", "user.email", "qualification@grokptah.invalid"],
    )?;
    git(&source, &["add", "src/framing.py", "src/decoder.py"])?;
    git(
        &source,
        &["commit", "-m", "Deliberately failing UTF-8 framing fixture"],
    )?;
    let base = git(&source, &["rev-parse", "HEAD"])?;
    let tree = git(&source, &["rev-parse", "HEAD^{tree}"])?;
    let value = json!({"schemaVersion":1,"requestId":Uuid::new_v4().to_string(),"baseSha":base,"baseTree":tree,"providerDispatchCountBeforeGoal":0});
    save(&marker, &value)?;
    Ok(value)
}

async fn call(
    server: &grokptah_agent_bridge::ControlServerHandle,
    control_token: &str,
    name: &str,
    args: Value,
) -> Result<Value> {
    let body: Value = reqwest::Client::new().post(format!("http://{}/mcp", server.addr))
        .bearer_auth(control_token).header("Accept", "application/json")
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}}))
        .send().await?.json().await?;
    ensure!(
        body.get("error").is_none() && body["result"]["isError"] != true,
        "qualification control denied: {body}"
    );
    Ok(body["result"]["structuredContent"].clone())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 3,
        "usage: real_managed_assignment ROOT ABSOLUTE_CLI readiness|execute|review|apply|discard"
    );
    let mode = args[2].as_str();
    ensure!(
        matches!(
            mode,
            "readiness" | "execute" | "review" | "apply" | "discard"
        ),
        "unsupported qualification action"
    );
    fs::create_dir_all(&args[0])?;
    let root = dunce::canonicalize(&args[0])?;
    let temporary = dunce::canonicalize(std::env::temp_dir())?;
    ensure!(
        root.starts_with("/private/tmp") || root.starts_with(temporary),
        "qualification must use a disposable temporary directory"
    );
    let cli = PathBuf::from(&args[1]);
    ensure!(
        cli.is_absolute() && cli.is_file(),
        "an installed absolute CLI path is required"
    );
    let mut marker = initialize(&root)?;
    ensure!(
        marker["schemaVersion"] == 1,
        "unsupported qualification record"
    );
    let source = root.join("source");
    let oracle = root.join("oracle");
    ensure!(
        git(&source, &["remote"])?.is_empty(),
        "qualification source must have no remotes"
    );
    ensure!(
        git(&source, &["rev-parse", "HEAD"])?
            == marker["baseSha"].as_str().context("missing base")?,
        "qualification base moved"
    );
    let runtime_home = root.join("host");
    set_grokptah_home_override(Some(runtime_home));
    let host = AgentHost::create(HostConfig {
        always_approve: false,
        ..Default::default()
    })?;
    host.start()?;
    let session = match marker["sessionId"].as_str() {
        Some(session) => Uuid::parse_str(session)?,
        None => {
            let lane = host.session_new_kind(SessionKind::Build)?;
            host.session_set_cwd(lane.id, &source)?;
            marker["sessionId"] = json!(lane.id);
            save(&root.join("qualification.json"), &marker)?;
            lane.id
        }
    };
    let agent = host.ensure_session_agent(session)?;
    let control_token = Uuid::new_v4().to_string();
    let orch = OrchestrationService::new(
        host.clone(),
        host.event_bus(),
        OrchStore::open(root.join("orchestration"))?,
        OrchestrationConfig {
            bearer_token: control_token.clone(),
            allowlist: WorkspaceAllowlist::new([source.clone()]),
            max_concurrent_runs: 1,
            bounds: RunBounds {
                max_prompt_bytes: 16000,
                max_rounds: 6,
                max_duration_ms: 180000,
                max_total_tokens: None,
            },
        },
    );
    let profiles = orch.store().root().join("check-profiles");
    // Apple's /usr/bin/python3 is an xcrun launcher that consults mutable
    // host caches. Select the public interpreter itself before confinement.
    let python = [
        "/Applications/Xcode.app/Contents/Developer/usr/bin/python3",
        "/Library/Developer/CommandLineTools/usr/bin/python3",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .context("trusted Python interpreter is unavailable")?;
    fs::create_dir_all(&profiles)?;
    save(
        &profiles.join("utf8-frame.json"),
        &json!({"profileId":"utf8-frame","profileRevision":1,"executable":oracle.join("check.sh"),"executableDigest":file_digest(&oracle.join("check.sh")),"oracleRoot":oracle,"oracleDigest":directory_digest(&oracle),"args":[],"cwd":"oracle","env":[{"key":"QUALIFICATION_PYTHON","value":python}],"timeoutMs":5000,"maxOutputBytes":16384,"maxOutputDirBytes":65536,"network":"none","confinementRevision":2}),
    )?;
    if marker["workId"].is_null() {
        let (check, mut authority, oracle_root) =
            grokptah_agent_bridge::resolve_check_profile(orch.store().root(), "utf8-frame")
                .map_err(|error| anyhow::anyhow!(error.message))?;
        authority.source_root = source.clone();
        let authority = authority.seal();
        let baseline = grokptah_agent_bridge::execute_required_checks_with_authority(
            &[check],
            &source,
            &oracle_root,
            Some(&authority),
        )
        .map_err(|error| anyhow::anyhow!(error.message))?;
        ensure!(
            baseline.len() == 1
                && baseline[0].outcome == "failed"
                && baseline[0].exit_code == Some(1),
            "qualification baseline must fail the actual host oracle"
        );
        save(
            &root.join("baseline.json"),
            &json!({"baseSha":marker["baseSha"],"checkResults":baseline,"confinementRevision":2}),
        )?;
    }
    for (name, value) in [
        (
            "GROKPTAH_MANAGED_GROK_EXECUTABLE",
            cli.display().to_string(),
        ),
        (
            "GROKPTAH_MANAGED_GROK_WORKSPACE",
            source.display().to_string(),
        ),
        (
            "GROKPTAH_MANAGED_GROK_ISOLATE",
            root.join("worker-root").display().to_string(),
        ),
        (
            "GROKPTAH_MANAGED_GROK_REPOSITORY_ID",
            "qualification:utf8-frame-v1".into(),
        ),
        (
            "GROKPTAH_MANAGED_GROK_REF",
            "refs/heads/qualification".into(),
        ),
        (
            "GROKPTAH_MANAGED_GROK_SHA",
            marker["baseSha"].as_str().context("base")?.into(),
        ),
    ] {
        unsafe {
            std::env::set_var(name, value);
        }
    }
    let setup = orch.configure_managed_grok_from_operator_env().await;
    let setup_error = setup.as_ref().err().map(|error| error.message.clone());
    let server = start_control_server(orch.clone(), 0).await?;
    let result: Result<Value> = async {
        let request = json!({"request_id":marker["requestId"],"session_id":session,"workspace":source,"agent_id":agent.agent_id,"objective":OBJECTIVE,"allowed_files":["src/framing.py","src/decoder.py"],"check_profile_id":"utf8-frame","budget_profile":"economy","mutation_mode":"isolated_review"});
        let mut report = json!({"baseSha":marker["baseSha"],"baseTree":marker["baseTree"],"operatorEntry":"production loopback MCP","cliDigest":file_digest(&cli),"setupError":setup_error,"mode":mode});
        if matches!(mode,"readiness"|"execute") && marker["workId"].is_null() {
            let readiness = call(&server,&control_token,"ptah_prepare_verified_change",request.clone()).await?;
            report["readiness"] = readiness.clone(); save(&root.join("readiness.json"),&report)?;
            if mode == "readiness" || readiness["readiness"]["ready"] != true { return Ok(report); }
            // Readiness is durably recorded before this sole admission.
            let started = call(&server,&control_token,"ptah_start_verified_change",request).await?;
            marker["workId"] = started["workId"].clone(); save(&root.join("qualification.json"),&marker)?;
            report["started"] = started;
            let work_id = marker["workId"].as_str().context("missing admitted Work")?;
            let start = Instant::now();
            loop {
                orch.drive_native_executor_once().await;
                let work = orch.store().load_work_item(work_id)?.context("missing Work")?;
                if !matches!(work.state, grokptah_agent_bridge::orchestration::WorkState::Queued | grokptah_agent_bridge::orchestration::WorkState::Leased | grokptah_agent_bridge::orchestration::WorkState::Running) { break; }
                if start.elapsed() > Duration::from_secs(195) { bail!("qualification did not settle within its finite supervision window"); }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        let Some(work_id) = marker["workId"].as_str() else { return Ok(report); };
        orch.drive_native_executor_once().await;
        let status_args = json!({"request_id":"qualification-status","session_id":session,"workspace":source,"work_id":work_id});
        let mut status = call(&server,&control_token,"ptah_verified_change_status",status_args.clone()).await?;
        if matches!(mode,"apply"|"discard") {
            let work = orch.store().load_work_item(work_id)?.context("missing Work")?;
            if mode == "apply" {
                ensure!(status["phases"]["checksPassed"] == true, "host verification must pass before approval");
                if work.approval.is_none() { call(&server,&control_token,"ptah_approve_work",json!({"request_id":"qualification-explicit-approve","session_id":session,"workspace":source,"work_id":work_id,"expected_revision":work.revision,"note":"Explicit approval of the inspected disposable qualification candidate"})).await?; }
            }
            let current = orch.store().load_work_item(work_id)?.context("missing Work")?;
            report["disposition"] = call(&server,&control_token,if mode == "apply" {"ptah_apply_verified_change"} else {"ptah_discard_verified_change"},json!({"request_id":format!("qualification-explicit-{mode}"),"session_id":session,"workspace":source,"work_id":work_id,"candidate_digest":status["candidateDigest"],"expected_revision":current.revision})).await?;
            status = call(&server,&control_token,"ptah_verified_change_status",status_args).await?;
        }
        report["status"] = status;
        report["work"] = json!(orch.store().load_work_item(work_id)?);
        report["attempts"] = json!(orch.store().list_work_attempts(Some(work_id))?);
        report["runs"] = json!(orch.store().list_runs()?);
        report["sourceDiff"] = json!(git(&source,&["diff","--stat"])?);
        report["sourceFramingDigest"] = json!(file_digest(&source.join("src/framing.py")));
        report["sourceDecoderDigest"] = json!(file_digest(&source.join("src/decoder.py")));
        Ok(report)
    }.await;
    let control_stop = server.stop_and_wait().await;
    let background_stop = orch.stop_background_tasks().await;
    let host_stop = host.shutdown().await;
    ensure!(
        control_stop.is_clean()
            && background_stop.fully_stopped
            && background_stop.errors.is_empty()
            && host_stop.is_clean(),
        "ordered qualification shutdown was not proved"
    );
    drop(orch);
    drop(host);
    set_grokptah_home_override(None);
    let report = result?;
    save(&root.join(format!("{mode}-report.json")), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
