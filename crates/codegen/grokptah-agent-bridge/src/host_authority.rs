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
    model: &str,
    turn_generation: u64,
    store: Option<OrchStore>,
    effect_lease_id: String,
    effect_scope: String,
) -> Result<xai_provider_attempt::CanonicalHostAuthority> {
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
        .map_err(|error| anyhow!("canonical auth authority unavailable: {error}"))?
        .ok_or_else(|| anyhow!("canonical auth authority is unavailable"))?;
    let credential_identity = credentials.qualification_identity_fingerprint();
    let auth_generation = u64::from_str_radix(&credential_identity[..16], 16)
        .unwrap_or(1)
        .max(1);
    let (principal_incarnation, capability_generation) = if let Some(agent_id) = agent_id {
        let agent = store
            .and_then(|store| store.load_agent(agent_id).ok().flatten())
            .ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        let owner = agent
            .owner_principal_id
            .unwrap_or_else(|| format!("credential-{credential_identity}"));
        let capability_generation = agent
            .spec
            .as_ref()
            .map(|spec| spec.revision)
            .ok_or_else(|| anyhow!("canonical capability authority is unavailable"))?;
        (
            format!("agent-{owner}-{agent_id}-auth-{credential_identity}"),
            capability_generation.max(1),
        )
    } else {
        // A non-Agent Lane still binds to the live credential principal. API
        // key and legacy records use the one-way credential identity digest;
        // OAuth refresh preserves this digest while principal rotation does
        // not.
        (
            format!("lane-{session_id}-auth-{credential_identity}"),
            turn_generation.max(1),
        )
    };

    xai_provider_attempt::CanonicalHostAuthority::from_trusted_host_adapter(
        principal_incarnation,
        auth_generation,
        capability_generation,
        effect_lease_id,
        effect_scope,
    )
    .map_err(|error| anyhow!("construct canonical host authority: {error}"))
}

pub(crate) fn fresh_effect_lease_id() -> String {
    format!("effect-lease-{}", Uuid::new_v4())
}
