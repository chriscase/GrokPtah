//! Assembly of the canonical host authority used by provider sends.
//!
//! This is the only bridge-side adapter that turns the live Agent/Lane
//! identity and capability revision into the opaque authority token consumed
//! by `xai-provider-attempt`.

use anyhow::{anyhow, Result};
use uuid::Uuid;

use crate::orchestration::OrchStore;

pub(crate) fn assemble(
    session_id: Uuid,
    agent_id: Option<&str>,
    turn_generation: u64,
    store: Option<OrchStore>,
) -> Result<xai_provider_attempt::CanonicalHostAuthority> {
    let (principal_incarnation, capability_generation) = if let Some(agent_id) = agent_id {
        let agent = store
            .and_then(|store| store.load_agent(agent_id).ok().flatten())
            .ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        let owner = agent
            .owner_principal_id
            .ok_or_else(|| anyhow!("canonical principal authority is unavailable"))?;
        let capability_generation = agent
            .spec
            .as_ref()
            .map(|spec| spec.revision)
            .ok_or_else(|| anyhow!("canonical capability authority is unavailable"))?;
        (
            format!("agent-{owner}-{agent_id}"),
            capability_generation.max(1),
        )
    } else {
        // Ephemeral desktop Lanes have no durable Agent owner. The host's
        // monotonic turn generation is their principal/auth incarnation and
        // capability generation; it is never reused within this host.
        (format!("lane-{session_id}"), turn_generation.max(1))
    };

    xai_provider_attempt::CanonicalHostAuthority::from_trusted_host_adapter(
        principal_incarnation,
        turn_generation.max(1),
        capability_generation,
        format!("effect-lease-{session_id}-{turn_generation}"),
        format!("effect-scope-{session_id}"),
    )
    .map_err(|error| anyhow!("construct canonical host authority: {error}"))
}
