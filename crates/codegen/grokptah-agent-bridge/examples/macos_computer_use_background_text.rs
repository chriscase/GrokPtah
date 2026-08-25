#[cfg(target_os = "macos")]
use std::collections::BTreeSet;

#[cfg(target_os = "macos")]
use chrono::{Duration, Utc};
#[cfg(target_os = "macos")]
use grokptah_agent_bridge::computer_use::SemanticElement;
#[cfg(target_os = "macos")]
use grokptah_agent_bridge::{
    set_grokptah_home_override, ActionClass, ActionGrant, AgentHost, ComputerAction,
    ComputerAuthorityToken, ComputerCapabilityTier, ComputerObservation,
    ComputerObservationPlatform, ComputerRun, ComputerStore, ComputerUseLimits, ComputerUseService,
    HostConfig, MacOsObservationPlatform, SemanticAction,
};
#[cfg(target_os = "macos")]
use uuid::Uuid;

#[cfg(target_os = "macos")]
const DEMO_BUNDLE_ID: &str = "com.chriscase.grokptah.computer-use-demo";

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::var("GROKPTAH_LIVE_BACKGROUND_COMPUTER_USE").as_deref() != Ok("1") {
        anyhow::bail!(
            "set GROKPTAH_LIVE_BACKGROUND_COMPUTER_USE=1 only for the repository disposable demo app"
        );
    }
    let platform = MacOsObservationPlatform::new_native()?;
    let status = platform.status();
    if !status.screen_recording.is_granted() || !status.accessibility.is_granted() {
        anyhow::bail!(
            "background smoke requires existing Screen Recording and Accessibility grants: {status:?}"
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
    if candidate.active {
        anyhow::bail!(
            "the disposable demo is foreground; put Terminal or another ordinary app in front and retry"
        );
    }

    let receipt = platform
        .measure_background_text_entry(
            &candidate.selection_token,
            "Project label",
            "grokptah-background-calibration-probe",
            true,
        )
        .await?;
    if receipt.target != candidate.target
        || receipt.supported_action_classes != BTreeSet::from([ActionClass::TextEntry])
    {
        anyhow::bail!("background calibration receipt did not preserve exact text-entry scope");
    }

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
        .bind_measured_background_target_service(
            &candidate.selection_token,
            &receipt.measurement_token,
            ComputerStore::open(temp.path().join("computer-use"))?,
        )
        .await?;
    let run = service.create_run(
        &Uuid::new_v4().to_string(),
        &caller,
        None,
        candidate.target,
        ComputerUseLimits {
            max_actions: 3,
            max_duration_secs: 120,
            max_observation_age_millis: 10_000,
            ..ComputerUseLimits::default()
        },
    )?;
    if run.capability_proof.tier() != ComputerCapabilityTier::MeasuredBackgroundSafeSemantic
        || run.capability_proof.semantic_actions()
        || !run.capability_proof.text_entry()
    {
        anyhow::bail!("background Run did not preserve its exact measured text-entry proof");
    }

    let (ready, observation) = authorize_and_observe(&service, &run, &caller).await?;
    let field = project_label_field(&observation)?;
    let original_value = field
        .value
        .clone()
        .ok_or_else(|| anyhow::anyhow!("demo Project label value was not readable"))?;
    let runtime_value = format!("background-runtime-proof-{}", Uuid::new_v4().simple());
    service
        .act(
            &Uuid::new_v4().to_string(),
            &caller,
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::SetValue {
                element_id: field.element_id.clone(),
                text: runtime_value.clone(),
            },
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (ready, observation) = authorize_and_observe(&service, &paused, &caller).await?;
    let field = project_label_field(&observation)?;
    if field.value.as_deref() != Some(runtime_value.as_str()) {
        anyhow::bail!("background runtime value was not visible in the fresh observation");
    }
    service
        .act(
            &Uuid::new_v4().to_string(),
            &caller,
            &ready.run_id,
            ready.version,
            &observation.observation_id,
            ComputerAction::SetValue {
                element_id: field.element_id.clone(),
                text: original_value.clone(),
            },
        )
        .await?;

    let paused = current_run(&service, &ready.run_id)?;
    let (_ready, observation) = authorize_and_observe(&service, &paused, &caller).await?;
    if project_label_field(&observation)?.value.as_deref() != Some(original_value.as_str()) {
        anyhow::bail!("background runtime smoke did not restore the disposable value");
    }
    println!(
        "native macOS measured-background text-entry smoke: ok (exact target, calibration restored, two runtime actions, final value restored)"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn project_label_field(observation: &ComputerObservation) -> anyhow::Result<&SemanticElement> {
    let mut matches = observation.elements.iter().filter(|element| {
        element.actions.contains(&SemanticAction::SetValue)
            && element.label.as_deref() == Some("Project label")
    });
    let field = matches
        .next()
        .ok_or_else(|| anyhow::anyhow!("demo Project label field was not observed"))?;
    if matches.next().is_some() {
        anyhow::bail!("demo Project label field was ambiguous");
    }
    Ok(field)
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
            BTreeSet::from([ActionClass::TextEntry]),
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
    eprintln!("native macOS background Computer Use smoke is available only on macOS");
}
