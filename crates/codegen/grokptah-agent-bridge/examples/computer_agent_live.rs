//! Opt-in live proof for the bounded Computer model proposal loop.
//!
//! This uses only the deterministic simulator. It does not open a workspace,
//! capture a window, request OS permissions, stage an approval, or execute an
//! action.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AgentHost, ComputerBackend, ComputerUseLimits, EffortLevel,
    HostConfig, SimulatorBackend,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LiveProof {
    ok: bool,
    model: String,
    qualification_source: String,
    qualification_ms: u128,
    proposal_ms: u128,
    proposal: grokptah_agent_bridge::ComputerAgentProposal,
    action_executed: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("GROKPTAH_LIVE_COMPUTER").as_deref() != Ok("1") {
        bail!("set GROKPTAH_LIVE_COMPUTER=1 to run the live simulator proof");
    }

    let private_home = tempfile::tempdir().context("create isolated GrokPtah home")?;
    set_grokptah_home_override(Some(private_home.path().to_path_buf()));

    let model = std::env::var("GROKPTAH_COMPUTER_MODEL")
        .unwrap_or_else(|_| HostConfig::default().default_model);
    let effort = parse_effort(
        &std::env::var("GROKPTAH_COMPUTER_EFFORT").unwrap_or_else(|_| "medium".into()),
    )?;
    let host = AgentHost::create(HostConfig {
        default_model: model.clone(),
        default_effort: effort,
        ..HostConfig::default()
    })
    .expect("acquire the GrokPtah instance lock");
    host.start()?;
    host.set_model(model.clone());
    host.set_effort(effort);
    let session = host.session_new()?;

    let qualification_started = Instant::now();
    let eligibility = host.qualify_computer_agent(session.id).await?;
    let qualification_ms = qualification_started.elapsed().as_millis();

    let simulator = SimulatorBackend::new();
    let observation = simulator
        .observe(
            "live-computer-model-proof",
            "live-computer-model-observation",
            &SimulatorBackend::demo_target(),
            &ComputerUseLimits::default(),
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let proposal_started = Instant::now();
    let proposal = host
        .propose_computer_action(
            session.id,
            "Enter Ada Lovelace in the visible Name field. Do not submit yet.",
            &observation,
        )
        .await?;
    let proposal_ms = proposal_started.elapsed().as_millis();

    println!(
        "{}",
        serde_json::to_string_pretty(&LiveProof {
            ok: true,
            model: eligibility.model,
            qualification_source: eligibility.source,
            qualification_ms,
            proposal_ms,
            proposal,
            action_executed: false,
        })?
    );
    host.stop()?;
    set_grokptah_home_override(None);
    Ok(())
}

fn parse_effort(value: &str) -> Result<EffortLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => Ok(EffortLevel::None),
        "minimal" => Ok(EffortLevel::Minimal),
        "low" => Ok(EffortLevel::Low),
        "medium" => Ok(EffortLevel::Medium),
        "high" => Ok(EffortLevel::High),
        "xhigh" => Ok(EffortLevel::Xhigh),
        "max" => Ok(EffortLevel::Max),
        _ => bail!(
            "GROKPTAH_COMPUTER_EFFORT must be none, minimal, low, medium, high, xhigh, or max"
        ),
    }
}
