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

use crate::orchestration::{authz::AuthContext, OrchStore};

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
    #[serde(default)]
    issued_effect_lease_ids: Vec<String>,
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifiedReconciliation {
    operator_id: String,
    provider_request_id: String,
    provider_effect_id: Option<String>,
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
    let capability = capability_lease(agent_id, store, effect_scope.clone(), turn_generation)?;
    let (revoked_effect_lease_ids, mut issued_effect_lease_ids) =
        match fs::read(authority_path(attempt_root, &effect_scope)) {
            Ok(bytes) => {
                let record = serde_json::from_slice::<AuthorityRecord>(&bytes)?;
                (
                    record.revoked_effect_lease_ids,
                    record.issued_effect_lease_ids,
                )
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(error) => return Err(error.into()),
        };
    let lease_id = capability.lease_id;
    issued_effect_lease_ids.push(lease_id.clone());
    write_authority(
        attempt_root,
        &AuthorityRecord {
            principal_incarnation: principal.incarnation,
            auth_generation: principal.auth_generation,
            capability_generation: capability.generation,
            effect_lease_id: lease_id,
            effect_scope: capability.scope,
            revoked_effect_lease_ids,
            issued_effect_lease_ids,
        },
        &effect_scope,
    )
}

pub(crate) fn refresh(
    session_id: Uuid,
    agent_id: Option<&str>,
    model: &str,
    turn_generation: u64,
    store: Option<OrchStore>,
    attempt_root: &Path,
    effect_scope: &str,
    rotate_capability: bool,
) -> Result<()> {
    let path = authority_path(attempt_root, effect_scope);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut current: AuthorityRecord = serde_json::from_slice(&bytes)?;
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
        .map_err(|error| anyhow!("canonical auth authority unavailable: {error}"))?
        .ok_or_else(|| anyhow!("canonical auth authority is unavailable"))?;
    let identity = credentials.qualification_identity_fingerprint();
    let principal = principal_ref(session_id, agent_id, &identity, store.clone())?;
    current.principal_incarnation = principal.incarnation;
    current.auth_generation = principal.auth_generation;
    current.capability_generation =
        current
            .capability_generation
            .max(capability_generation(agent_id, store, turn_generation)?);
    if rotate_capability {
        current.capability_generation = current.capability_generation.saturating_add(1);
    }
    write_authority(attempt_root, &current, effect_scope)
}

pub(crate) fn revoke_scope(attempt_root: &Path, effect_scope: &str) -> Result<()> {
    let path = authority_path(attempt_root, effect_scope);
    let bytes = fs::read(path)?;
    let mut current: AuthorityRecord = serde_json::from_slice(&bytes)?;
    let issued = current.issued_effect_lease_ids.clone();
    for lease in issued.into_iter().chain([current.effect_lease_id.clone()]) {
        if !current
            .revoked_effect_lease_ids
            .iter()
            .any(|item| item == &lease)
        {
            current.revoked_effect_lease_ids.push(lease);
        }
    }
    write_authority(attempt_root, &current, effect_scope)
}

pub(crate) fn write_verified_reconciliation(
    attempt_root: &Path,
    attempt_id: &str,
    operator: &AuthContext,
    provider_request_id: &str,
    provider_effect_id: Option<&str>,
) -> Result<()> {
    if operator.token_id.trim().is_empty()
        || attempt_id.trim().is_empty()
        || provider_request_id.trim().is_empty()
        || provider_effect_id.is_some_and(|effect| effect.trim().is_empty())
    {
        return Err(anyhow!("verified reconciliation fields are incomplete"));
    }
    let directory = attempt_root.join("reconciliation");
    fs::create_dir_all(&directory)?;
    let temporary = directory.join(format!(".{attempt_id}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let record = VerifiedReconciliation {
        operator_id: operator.token_id.clone(),
        provider_request_id: provider_request_id.into(),
        provider_effect_id: provider_effect_id.map(str::to_owned),
    };
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, directory.join(format!("{attempt_id}.json")))?;
    Ok(())
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
    let generation = capability_generation(agent_id, store, turn_generation)?;
    Ok(CapabilityEffectLease {
        generation,
        lease_id: format!("effect-lease-{}", Uuid::new_v4()),
        scope,
    })
}

fn capability_generation(
    agent_id: Option<&str>,
    store: Option<OrchStore>,
    turn_generation: u64,
) -> Result<u64> {
    if let Some(agent_id) = agent_id {
        let store = store.ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        let agent = store
            .load_agent(agent_id)?
            .ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        if !agent.state.is_active_identity() {
            return Err(anyhow!("terminal Agent authority cannot send"));
        }
        Ok(agent
            .spec
            .as_ref()
            .map(|spec| spec.revision.max(1))
            .ok_or_else(|| anyhow!("canonical capability authority is unavailable"))?)
    } else {
        Ok(turn_generation.max(1))
    }
}

fn authority_path(root: &Path, scope: &str) -> PathBuf {
    root.join("canonical-authorities")
        .join(format!("{scope}.json"))
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
            issued_effect_lease_ids: vec![effect_lease_id.into()],
        },
        scope,
    )
}
