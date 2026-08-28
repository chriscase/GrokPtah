//! Assembly of the canonical host authority used by provider sends.
//!
//! This is the only bridge-side adapter that turns the live Agent/Lane
//! identity and capability lease into a durable authority snapshot consumed
//! by `xai-provider-attempt`.

use anyhow::{anyhow, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::orchestration::{AuthContext, OrchStore};

const AUTHORITY_PUBLIC_KEY_FILE: &str = ".authority-public-key";
const LEASE_BATCH_SIZE: usize = 64;

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

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedAuthorityRecord {
    #[serde(flatten)]
    payload: AuthorityRecord,
    signature: String,
}

struct PrincipalRef {
    incarnation: String,
    auth_generation: AuthenticationGeneration,
}

struct AuthenticationGeneration(u64);

impl AuthenticationGeneration {
    fn from_credential_identity(identity: &str) -> Self {
        Self(u64::from_str_radix(&identity[..16], 16).unwrap_or(1).max(1))
    }
}

struct CapabilityEffectLease {
    generation: u64,
    lease_id: String,
    scope: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifiedReconciliation {
    attempt_id: String,
    operator_id: String,
    provider_request_id: String,
    provider_effect_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedVerifiedReconciliation {
    #[serde(flatten)]
    payload: VerifiedReconciliation,
    signature: String,
}

pub(crate) fn assemble(
    session_id: Uuid,
    agent_id: Option<&str>,
    model: &str,
    turn_generation: u64,
    store: Option<OrchStore>,
    attempt_root: &Path,
) -> Result<()> {
    let effect_scope = scope(session_id);
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
        .map_err(|error| anyhow!("canonical auth authority unavailable: {error}"))?
        .ok_or_else(|| anyhow!("canonical auth authority is unavailable"))?;
    let identity = credentials.qualification_identity_fingerprint();
    let principal = principal_ref(agent_id, &identity, store.clone())?;
    let capability = capability_lease(agent_id, store, effect_scope.clone(), turn_generation)?;
    let (revoked_effect_lease_ids, mut issued_effect_lease_ids) =
        match read_authority(attempt_root, &effect_scope) {
            Ok(record) => (
                record.revoked_effect_lease_ids,
                record.issued_effect_lease_ids,
            ),
            Err(error) if is_not_found(&error) => (Vec::new(), Vec::new()),
            Err(error) => return Err(error),
        };
    let lease_id = capability.lease_id;
    issued_effect_lease_ids.push(lease_id.clone());
    issued_effect_lease_ids
        .extend((1..LEASE_BATCH_SIZE).map(|_| format!("effect-lease-{}", Uuid::new_v4())));
    write_authority(
        attempt_root,
        &AuthorityRecord {
            principal_incarnation: principal.incarnation,
            auth_generation: principal.auth_generation.0,
            capability_generation: capability.generation,
            effect_lease_id: lease_id,
            effect_scope: capability.scope,
            revoked_effect_lease_ids,
            issued_effect_lease_ids,
        },
        &effect_scope,
    )
}

pub(crate) fn scope(session_id: Uuid) -> String {
    format!("provider-session-{session_id}")
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn refresh(
    agent_id: Option<&str>,
    model: &str,
    turn_generation: u64,
    store: Option<OrchStore>,
    attempt_root: &Path,
    effect_scope: &str,
    rotate_capability: bool,
) -> Result<()> {
    let mut current = match read_authority(attempt_root, effect_scope) {
        Ok(record) => record,
        Err(error) if is_not_found(&error) => return Ok(()),
        Err(error) => return Err(error),
    };
    let credentials = crate::auth_store::resolve_wire_credentials_for_model(model)
        .map_err(|error| anyhow!("canonical auth authority unavailable: {error}"))?
        .ok_or_else(|| anyhow!("canonical auth authority is unavailable"))?;
    let identity = credentials.qualification_identity_fingerprint();
    let principal = principal_ref(agent_id, &identity, store.clone())?;
    current.principal_incarnation = principal.incarnation;
    current.auth_generation = principal.auth_generation.0;
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
    let mut current = read_authority(attempt_root, effect_scope)?;
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
    receipt: &crate::host::VerifiedProviderReceipt,
) -> Result<()> {
    if operator.token_id.trim().is_empty() || attempt_id.trim().is_empty() {
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
        attempt_id: attempt_id.into(),
        operator_id: operator.token_id.clone(),
        provider_request_id: receipt.provider_request_id().into(),
        provider_effect_id: receipt.provider_effect_id().map(str::to_owned),
    };
    let signing_key = signing_key(attempt_root)?;
    let payload = serde_json::to_vec(&record)?;
    let signature = signing_key.sign(&payload);
    file.write_all(&serde_json::to_vec(&SignedVerifiedReconciliation {
        payload: record,
        signature: hex(signature.to_bytes().as_slice()),
    })?)?;
    // Reconciliation records contain operator identity and provider truth.
    // Apply the private mode to the temporary before the atomic rename so the
    // verifier's permission check succeeds even when the process umask is
    // permissive.
    set_private_permissions(&file)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, directory.join(format!("{attempt_id}.json")))?;
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn principal_ref(
    agent_id: Option<&str>,
    credential_identity: &str,
    store: Option<OrchStore>,
) -> Result<PrincipalRef> {
    let incarnation = if let Some(agent_id) = agent_id {
        let store = store.ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        let agent = store
            .load_agent(agent_id)?
            .ok_or_else(|| anyhow!("canonical Agent authority is unavailable"))?;
        if !agent.state.is_active_identity() {
            return Err(anyhow!("terminal Agent authority cannot send"));
        }
        agent
            .owner_principal_id
            .unwrap_or_else(|| credential_identity.to_owned())
    } else {
        credential_identity.to_owned()
    };
    Ok(PrincipalRef {
        incarnation,
        auth_generation: AuthenticationGeneration::from_credential_identity(credential_identity),
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

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// Initialize the host-owned signing key before the common ledger is exposed.
///
/// The common attempt crate only verifies this trust anchor; it must never
/// create a key as a side effect of opening or reading a caller-selected
/// ledger root.
pub(crate) fn initialize(root: &Path) -> Result<()> {
    let directory = root.join("canonical-authorities");
    fs::create_dir_all(&directory)?;
    let key_path = directory.join(".authority-signing-key");
    match fs::symlink_metadata(&key_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow!("canonical authority signing key is a symlink"));
        }
        Ok(metadata) => {
            #[cfg(unix)]
            if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
                return Err(anyhow!(
                    "canonical authority signing key permissions are too broad"
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if directory.join(AUTHORITY_PUBLIC_KEY_FILE).exists() {
                return Err(anyhow!("canonical authority signing key is unavailable"));
            }
            let mut seed = [0u8; 32];
            seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
            seed[16..].copy_from_slice(Uuid::new_v4().as_bytes());
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&key_path)?;
            file.write_all(&seed)?;
            set_private_permissions(&file)?;
            file.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    let key = {
        let bytes = fs::read(&key_path)?;
        let seed: [u8; 32] = bytes
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("canonical authority signing key is invalid"))?;
        SigningKey::from_bytes(&seed)
    };
    let public_key_path = directory.join(AUTHORITY_PUBLIC_KEY_FILE);
    match fs::symlink_metadata(&public_key_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(anyhow!("canonical authority public key is a symlink"));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&public_key_path)?;
            file.write_all(&key.verifying_key().to_bytes())?;
            set_private_permissions(&file)?;
            file.sync_all()?;
        }
        Err(error) => return Err(error.into()),
    }
    let _ = read_public_key(root)?;
    Ok(())
}

fn write_authority(root: &Path, record: &AuthorityRecord, scope: &str) -> Result<()> {
    let directory = root.join("canonical-authorities");
    fs::create_dir_all(&directory)?;
    let signing_key = signing_key(root)?;
    let payload = serde_json::to_vec(record)?;
    let signature = signing_key.sign(&payload);
    let temporary = directory.join(format!(".{scope}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(&serde_json::to_vec(&SignedAuthorityRecord {
        payload: record.clone(),
        signature: hex(signature.to_bytes().as_slice()),
    })?)?;
    set_private_permissions(&file)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, authority_path(root, scope))?;
    if let Ok(directory) = File::open(directory) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn read_authority(root: &Path, scope: &str) -> Result<AuthorityRecord> {
    let path = authority_path(root, scope);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!("canonical authority record is a symlink"));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
        return Err(anyhow!(
            "canonical authority record permissions are too broad"
        ));
    }
    let signed: SignedAuthorityRecord = serde_json::from_slice(&fs::read(path)?)?;
    let public_key = read_public_key(root)?;
    let signature_bytes =
        decode_hex(&signed.signature).ok_or_else(|| anyhow!("canonical authority signature"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| anyhow!("canonical authority signature"))?;
    let payload = serde_json::to_vec(&signed.payload)?;
    public_key
        .verify(&payload, &signature)
        .map_err(|_| anyhow!("canonical authority signature verification failed"))?;
    Ok(signed.payload)
}

fn signing_key(root: &Path) -> Result<SigningKey> {
    let directory = root.join("canonical-authorities");
    let key_path = directory.join(".authority-signing-key");
    let metadata = fs::symlink_metadata(&key_path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!("canonical authority signing key is a symlink"));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
        return Err(anyhow!(
            "canonical authority signing key permissions are too broad"
        ));
    }
    let bytes = fs::read(&key_path)?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("canonical authority signing key is invalid"))?;
    let key = SigningKey::from_bytes(&seed);
    let public_key = key.verifying_key().to_bytes();
    if read_public_key(root)?.to_bytes() != public_key {
        return Err(anyhow!("canonical authority public key mismatch"));
    }
    Ok(key)
}

fn read_public_key(root: &Path) -> Result<ed25519_dalek::VerifyingKey> {
    let path = root
        .join("canonical-authorities")
        .join(AUTHORITY_PUBLIC_KEY_FILE);
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!("canonical authority public key is a symlink"));
    }
    #[cfg(unix)]
    if std::os::unix::fs::PermissionsExt::mode(&metadata.permissions()) & 0o077 != 0 {
        return Err(anyhow!(
            "canonical authority public key permissions are too broad"
        ));
    }
    let bytes = fs::read(path)?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("canonical authority public key is invalid"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&bytes)
        .map_err(|_| anyhow!("canonical authority public key is invalid"))
}

fn set_private_permissions(file: &File) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|chunk| {
            let high = (chunk[0] as char).to_digit(16)? as u8;
            let low = (chunk[1] as char).to_digit(16)? as u8;
            Some((high << 4) | low)
        })
        .collect()
}

#[cfg(test)]
pub(crate) fn write_test_snapshot(
    root: &Path,
    scope: &str,
    principal_incarnation: &str,
    auth_generation: u64,
    capability_generation: u64,
    effect_lease_id: &str,
) -> Result<()> {
    initialize(root)?;
    let mut issued_effect_lease_ids = vec![effect_lease_id.into()];
    issued_effect_lease_ids
        .extend((1..LEASE_BATCH_SIZE).map(|_| format!("test-effect-lease-{}", Uuid::new_v4())));
    write_authority(
        root,
        &AuthorityRecord {
            principal_incarnation: principal_incarnation.into(),
            auth_generation,
            capability_generation,
            effect_lease_id: effect_lease_id.into(),
            effect_scope: scope.into(),
            revoked_effect_lease_ids: Vec::new(),
            issued_effect_lease_ids,
        },
        scope,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn signed_reconciliation_record_is_private_after_atomic_write() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        initialize(root.path()).unwrap();
        let operator = AuthContext {
            token_id: "operator-1".into(),
            owner_id: "owner-1".into(),
        };
        let receipt = crate::host::VerifiedProviderReceipt::from_provider_response(
            "request-1",
            None::<String>,
        )
        .unwrap();

        write_verified_reconciliation(root.path(), "attempt-1", &operator, &receipt).unwrap();

        let path = root.path().join("reconciliation/attempt-1.json");
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }

    #[cfg(unix)]
    #[test]
    fn initialize_rejects_a_world_readable_signing_key() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        initialize(root.path()).unwrap();
        let key_path = root
            .path()
            .join("canonical-authorities/.authority-signing-key");
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

        let error = initialize(root.path()).unwrap_err();
        assert!(error.to_string().contains("signing key permissions"));
    }
}
