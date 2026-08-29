//! Opt-in live proof for the bounded Computer model proposal loop.
//!
//! This uses only the deterministic simulator. It does not open a workspace,
//! capture a window, request OS permissions, stage an approval, or execute an
//! action.

use std::time::Instant;

use anyhow::{bail, Context, Result};
use grokptah_agent_bridge::{
    set_grokptah_home_override, AdaptiveProfile, AgentHost, ComputerBackend, ComputerUseLimits,
    EffortLevel, HostConfig, SimulatorBackend, TurnPermit,
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
    /// Raw, untrusted provider output. This proof exercises the model call
    /// only. The bytes carry no authority and were never sealed against a live
    /// run, so nothing here could stage or complete anything (#457).
    raw_proposal: grokptah_agent_bridge::RawModelProposal,
    /// The adaptive profile the turn ran under, and what its bounded view
    /// actually cost. Provider token figures stay `null` unless the provider
    /// reported them.
    profile: AdaptiveProfile,
    observation_bytes: u64,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    proposal_sealed: bool,
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
    });
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
    // The adaptive layer admits turns against a *durable* Computer Run record,
    // and this proof deliberately has no such run: it exercises the provider
    // round-trip against the deterministic simulator and nothing else. Building
    // the permit directly here keeps that boundary visible — it grants no
    // authority, because the bytes it produces have none either.
    let objective = "Enter Ada Lovelace in the visible Name field. Do not submit yet.";
    let permit = TurnPermit::unbound(AdaptiveProfile::Economy);
    let proposal_started = Instant::now();
    let attempt = host
        .propose_computer_action(session.id, objective, &observation, &permit)
        .await?;
    let proposal_ms = proposal_started.elapsed().as_millis();
    let raw_proposal = attempt.outcome?;
    let observation_bytes = attempt.rendered.bytes;
    let prompt_tokens = attempt.prompt_tokens;
    let completion_tokens = attempt.completion_tokens;

    println!(
        "{}",
        serde_json::to_string_pretty(&LiveProof {
            ok: true,
            model: eligibility.model,
            qualification_source: eligibility.source,
            qualification_ms,
            proposal_ms,
            raw_proposal,
            observation_bytes,
            prompt_tokens,
            completion_tokens,
            profile: AdaptiveProfile::Economy,
            proposal_sealed: false,
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
