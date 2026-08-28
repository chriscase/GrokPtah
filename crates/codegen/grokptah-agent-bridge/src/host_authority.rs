//! Assembly of the canonical host authority used by provider sends.
//!
//! This is the only bridge-side adapter that turns the live Agent/Lane
//! identity and capability revision into the opaque authority token consumed
//! by `xai-provider-attempt`.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::orchestration::OrchStore;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuthorityRecord {
    principal_incarnation: String,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: String,
    effect_scope: String,
    #[serde(default)]
    revoked_effect_lease_ids: Vec<String>,
}

struct PrincipalRef {
    incarnation: String,
    auth_generation: u64,
}

struct CapabilityEffectLease {
    generation: u64,
    lease_id: String,
    scope: String,
}

pub(crate) fn assemble(
    session_id: Uuid,
    agent_id: Option<&str>,
    model: &str,
    turn_generation: u64,
    store: Option<OrchStore>,
    attempt_root: &Path,
    effect_scope: String,
) -> Result<()> {
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
        .map_err(|error| anyhow!("canonical auth authority unavailable: {error}"))?
        .ok_or_else(|| anyhow!("canonical auth authority is unavailable"))?;
    let identity = credentials.qualification_identity_fingerprint();
    let principal = principal_ref(session_id, agent_id, &identity, store.clone())?;
    let capability =
        capability_lease(agent_id, store, effect_scope.clone(), turn_generation)?;
    write_authority(
        attempt_root,
        &AuthorityRecord {
            principal_incarnation: principal.incarnation,
            auth_generation: principal.auth_generation,
            capability_generation: capability.generation,
            effect_lease_id: capability.lease_id,
            effect_scope: capability.scope,
            revoked_effect_lease_ids: Vec::new(),
        },
        &effect_scope,
    )
}

fn principal_ref(
    session_id: Uuid,
    agent_id: Option<&str>,
    credential_identity: &str,
    store: Option<OrchStore>,
) -> Result<PrincipalRef> {
    let owner = agent_id
        .and_then(|id| store.as_ref().and_then(|s| s.load_agent(id).ok().flatten()))
        .and_then(|agent| agent.owner_principal_id)
        .unwrap_or_else(|| credential_identity.to_owned());
    let prefix = agent_id
        .map(|id| format!("agent-{id}"))
        .unwrap_or_else(|| format!("lane-{session_id}"));
    Ok(PrincipalRef {
        incarnation: format!("{prefix}-principal-{owner}"),
        auth_generation: u64::from_str_radix(&credential_identity[..16], 16)
            .unwrap_or(1)
            .max(1),
    })
}

fn capability_lease(
    agent_id: Option<&str>,
    store: Option<OrchStore>,
    scope: String,
    turn_generation: u64,
) -> Result<CapabilityEffectLease> {
    let generation = if let Some(agent_id) = agent_id {
        let store = store.ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        let agent = store
            .load_agent(agent_id)?
            .ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        if !agent.state.is_active_identity() {
            return Err(anyhow!("terminal Agent authority cannot send"));
        }
        agent
            .spec
            .as_ref()
            .map(|spec| spec.revision.max(1))
            .ok_or_else(|| anyhow!("canonical capability authority is unavailable"))?
    } else {
        turn_generation.max(1)
    };
    Ok(CapabilityEffectLease {
        generation,
        lease_id: format!("effect-lease-{}", Uuid::new_v4()),
        scope,
    })
}

fn authority_path(root: &Path, scope: &str) -> PathBuf {
    root.join("canonical-authorities").join(format!("{scope}.json"))
}

fn write_authority(root: &Path, record: &AuthorityRecord, scope: &str) -> Result<()> {
    let directory = root.join("canonical-authorities");
    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(".{scope}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, authority_path(root, scope))?;
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

pub(crate) fn write_snapshot(
    root: &Path,
    scope: &str,
    principal_incarnation: &str,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: &str,
) -> Result<()> {
    write_authority(
        root,
        &AuthorityRecord {
            principal_incarnation: principal_incarnation.into(),
            auth_generation,
            capability_generation,
            effect_lease_id: effect_lease_id.into(),
            effect_scope: scope.into(),
            revoked_effect_lease_ids: Vec::new(),
        },
        scope,
    )
}
