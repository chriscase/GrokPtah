#[cfg(target_os = "macos")]
use std::collections::BTreeSet;

#[cfg(target_os = "macos")]
use chrono::{Duration, Utc};
#[cfg(target_os = "macos")]
use grokptah_agent_bridge::{
    ActionClass, ActionGrant, ComputerAction, ComputerObservation, ComputerObservationPlatform,
    ComputerRun, ComputerStore, ComputerUseLimits, ComputerUseService, GrantIssuer,
    MacOsObservationPlatform, SemanticAction,
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
    let backend = platform.bind_target(&candidate.selection_token).await?;
    let temp = tempfile::tempdir()?;
    let service = ComputerUseService::new(
        backend,
        ComputerStore::open(temp.path().join("computer-use"))?,
    );
    let run = service.create_run(
        &Uuid::new_v4().to_string(),
        Uuid::new_v4(),
        candidate.target,
        ComputerUseLimits {
            max_actions: 4,
            max_duration_secs: 120,
            max_observation_age_millis: 10_000,
            ..ComputerUseLimits::default()
        },
    )?;

    let (ready, observation) = authorize_and_observe(&service, &run).await?;
    service
        .act(
            &Uuid::new_v4().to_string(),
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::ActivateTarget,
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (ready, observation) = authorize_and_observe(&service, &paused).await?;
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
    let (ready, observation) = authorize_and_observe(&service, &paused).await?;
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
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::Invoke {
                element_id: button.element_id.clone(),
            },
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (_ready, observation) = authorize_and_observe(&service, &paused).await?;
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
) -> anyhow::Result<(ComputerRun, ComputerObservation)> {
    let now = Utc::now();
    let authorized = service.authorize(
        &Uuid::new_v4().to_string(),
        &run.run_id,
        run.version,
        ActionGrant {
            grant_id: Uuid::new_v4().to_string(),
            run_id: run.run_id.clone(),
            target: run.target.clone(),
            action_classes: BTreeSet::from([ActionClass::Semantic, ActionClass::TextEntry]),
            issued_by: GrantIssuer::LocalUser,
            issued_at: now,
            expires_at: now + Duration::seconds(30),
            uses_remaining: Some(1),
            revoked_at: None,
        },
    )?;
    let observation = service
        .observe(
            &Uuid::new_v4().to_string(),
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
