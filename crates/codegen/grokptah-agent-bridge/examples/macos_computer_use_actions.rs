#[cfg(target_os = "macos")]
use std::collections::BTreeSet;

#[cfg(target_os = "macos")]
use chrono::{Duration, Utc};
#[cfg(target_os = "macos")]
use grokptah_agent_bridge::{
    set_grokptah_home_override, ActionClass, ActionGrant, AgentHost, ComputerAction,
    ComputerAuthorityToken, ComputerObservation, ComputerObservationPlatform, ComputerRun,
    ComputerStore, ComputerUseLimits, ComputerUseService, HostConfig, MacOsObservationPlatform,
    SemanticAction,
};
#[cfg(target_os = "macos")]
use uuid::Uuid;

#[cfg(target_os = "macos")]
const DEMO_BUNDLE_ID: &str = "com.chriscase.grokptah.computer-use-demo";

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("GROKPTAH_LIVE_COMPUTER_USE").as_deref() != Ok("1") {
        anyhow::bail!(
            "set GROKPTAH_LIVE_COMPUTER_USE=1 only after launching the repository demo app"
        );
    }
    let platform = MacOsObservationPlatform::new_native()?;
    let status = platform.status();
    if !status.screen_recording.is_granted() || !status.accessibility.is_granted() {
        anyhow::bail!(
            "live smoke requires existing Screen Recording and Accessibility grants: {status:?}"
        );
    }
    let candidate = platform
        .list_targets()
        .await?
        .into_iter()
        .find(|candidate| {
            candidate.target.app_id == DEMO_BUNDLE_ID && candidate.on_screen && !candidate.minimized
        })
        .ok_or_else(|| anyhow::anyhow!("repository Computer Use demo window is not running"))?;
    let temp = tempfile::tempdir()?;
    set_grokptah_home_override(Some(temp.path().join(".grokptah")));
    let host = AgentHost::create(HostConfig {
        always_approve: true,
        ..HostConfig::default()
    });
    host.start()?;
    let session = host.session_new()?;
    let caller = host.computer_operator_token(session.id)?;
    let service = platform
        .bind_target_service(
            &candidate.selection_token,
            ComputerStore::open(temp.path().join("computer-use"))?,
        )
        .await?;
    // Live smoke has no AgentHost session cwd. Fail closed with None rather
    // than inventing a workspace from process cwd or the demo bundle path.
    let run = service.create_run(
        &Uuid::new_v4().to_string(),
        &caller,
        None,
        candidate.target,
        ComputerUseLimits {
            max_actions: 4,
            max_duration_secs: 120,
            max_observation_age_millis: 10_000,
            ..ComputerUseLimits::default()
        },
    )?;

    let (ready, observation) = authorize_and_observe(&service, &run, &caller).await?;
    service
        .act(
            &Uuid::new_v4().to_string(),
            &caller,
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::ActivateTarget,
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (ready, observation) = authorize_and_observe(&service, &paused, &caller).await?;
    let field = observation
        .elements
        .iter()
        .find(|element| {
            element.actions.contains(&SemanticAction::SetValue)
                && element.label.as_deref() == Some("Project label")
        })
        .ok_or_else(|| anyhow::anyhow!("demo Project label field was not observed"))?;
    service
        .act(
            &Uuid::new_v4().to_string(),
            &caller,
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::SetValue {
                element_id: field.element_id.clone(),
                text: "native-semantic-proof".into(),
            },
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (ready, observation) = authorize_and_observe(&service, &paused, &caller).await?;
    let button = observation
        .elements
        .iter()
        .find(|element| {
            element.actions.contains(&SemanticAction::Invoke)
                && element.label.as_deref() == Some("Submit fixture")
        })
        .ok_or_else(|| anyhow::anyhow!("demo Submit fixture button was not observed"))?;
    service
        .act(
            &Uuid::new_v4().to_string(),
            &caller,
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::Invoke {
                element_id: button.element_id.clone(),
            },
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (_ready, observation) = authorize_and_observe(&service, &paused, &caller).await?;
    let submitted = observation.elements.iter().any(|element| {
        element
            .label
            .as_deref()
            .is_some_and(|label| label.contains("Submitted native-semantic-proof"))
            || element
                .value
                .as_deref()
                .is_some_and(|value| value.contains("Submitted native-semantic-proof"))
    });
    if !submitted {
        anyhow::bail!("demo postcondition was not visible in the fresh observation");
    }
    println!("native macOS Computer Use semantic smoke: ok");
    Ok(())
}

#[cfg(target_os = "macos")]
async fn authorize_and_observe(
    service: &ComputerUseService,
    run: &ComputerRun,
    caller: &ComputerAuthorityToken,
) -> anyhow::Result<(ComputerRun, ComputerObservation)> {
    let now = Utc::now();
    let authorized = service.authorize(
        &Uuid::new_v4().to_string(),
        caller,
        &run.run_id,
        run.version,
        ActionGrant::for_run(
            run,
            BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            now,
            now + Duration::seconds(30),
            Some(1),
        ),
    )?;
    let observation = service
        .observe(
            &Uuid::new_v4().to_string(),
            caller,
            &authorized.run_id,
            authorized.version,
        )
        .await?;
    Ok((current_run(service, &authorized.run_id)?, observation))
}

#[cfg(target_os = "macos")]
fn current_run(service: &ComputerUseService, run_id: &str) -> anyhow::Result<ComputerRun> {
    service
        .get_run(run_id)?
        .ok_or_else(|| anyhow::anyhow!("Computer Run disappeared"))
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("native macOS Computer Use smoke is available only on macOS");
}
