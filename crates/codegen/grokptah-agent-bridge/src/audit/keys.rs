//! Audit chain keys: HMAC-SHA256 and domain-separated subkey derivation (#443).
//!
//! HMAC is implemented over the crate's existing `sha2` dependency rather than
//! pulling in the `hmac` crate, so the audit authority adds zero entries to
//! `crates/codegen/grokptah-agent-bridge/Cargo.lock` and `--locked` checks stay
//! reproducible offline.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{AuditError, AuditResult, PoisonReason};

const BLOCK: usize = 64;

/// Domain separation labels. Changing one of these invalidates every existing
/// tag produced under it, which is why they are constants and never formatted.
const LABEL_CHAIN: &[u8] = b"grokptah-audit.v2/chain";
const LABEL_MANIFEST: &[u8] = b"grokptah-audit.v2/manifest";
const LABEL_ANCHOR: &[u8] = b"grokptah-audit.v2/anchor";
const LABEL_SEAL: &[u8] = b"grokptah-audit.v2/seal";
const LABEL_ACTOR: &[u8] = b"grokptah-audit.v2/actor";
const LABEL_KEY_ID: &[u8] = b"grokptah-audit.v2/keyid";
const LABEL_INSTALL_ID: &[u8] = b"grokptah-audit.v2/install-id";
const LABEL_GENESIS: &[u8] = b"grokptah-audit.v2/genesis";
const LABEL_IMPORT_SEAL: &[u8] = b"grokptah-audit.v2/import-seal";

pub(crate) fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut normalized = [0u8; BLOCK];
    if key.len() > BLOCK {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for ((inner, outer), byte) in ipad.iter_mut().zip(opad.iter_mut()).zip(normalized.iter()) {
        *inner ^= *byte;
        *outer ^= *byte;
    }
    let mut inner_hash = Sha256::new();
    inner_hash.update(ipad);
    inner_hash.update(data);
    let inner_hash = inner_hash.finalize();
    let mut outer_hash = Sha256::new();
    outer_hash.update(opad);
    outer_hash.update(inner_hash);
    outer_hash.finalize().into()
}

pub(crate) fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    hex32(&digest)
}

/// Derived audit keys. The installation key never leaves this struct, and no
/// subkey is ever written to disk or into a projection.
#[derive(Clone)]
pub struct AuditKeys {
    root_material: Vec<u8>,
    chain: [u8; 32],
    manifest: [u8; 32],
    anchor: [u8; 32],
    seal: [u8; 32],
    actor: [u8; 32],
    key_id: String,
    installation_id: String,
    epoch: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKeyMode {
    PackagedDesktop,
    HeadlessService,
    ExternalConsumer,
}

pub trait AuditKeyProvider: Send + Sync + std::fmt::Debug {
    fn keyring(&self) -> Vec<Arc<AuditKeys>>;
    fn rotate(&self, current: &AuditKeys) -> AuditResult<Arc<AuditKeys>>;
}

/// Key custody boundary used by the shipped store. The mode is observable
/// only as a label; the key path and key bytes never appear in this value's
/// debug output or in audit status/projections.
#[derive(Clone)]
pub struct AuditKeyCustody {
    mode: AuditKeyMode,
    provider: Arc<dyn AuditKeyProvider>,
}

impl std::fmt::Debug for AuditKeyCustody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let key_id = self
            .provider
            .keyring()
            .first()
            .map(|key| key.key_id())
            .unwrap_or("unavailable");
        f.debug_struct("AuditKeyCustody")
            .field("mode", &self.mode)
            .field("keyId", &key_id)
            .finish()
    }
}

impl AuditKeyCustody {
    pub fn packaged_desktop(root: &Path) -> AuditResult<Self> {
        Self::file_backed(root, AuditKeyMode::PackagedDesktop)
    }

    pub fn headless_service(root: &Path) -> AuditResult<Self> {
        Self::file_backed(root, AuditKeyMode::HeadlessService)
    }

    /// External consumers must supply a provider that owns key retrieval and
    /// rotation. The ledger never persists or derives the next epoch for an
    /// external consumer.
    pub fn external_consumer(provider: Arc<dyn AuditKeyProvider>) -> AuditResult<Self> {
        if provider.keyring().is_empty() {
            return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
        }
        Ok(Self {
            mode: AuditKeyMode::ExternalConsumer,
            provider,
        })
    }

    pub fn mode(&self) -> AuditKeyMode {
        self.mode
    }

    pub(crate) fn all_keys(&self) -> Vec<Arc<AuditKeys>> {
        self.provider.keyring()
    }

    pub(crate) fn provider(&self) -> Arc<dyn AuditKeyProvider> {
        Arc::clone(&self.provider)
    }

    fn file_backed(root: &Path, mode: AuditKeyMode) -> AuditResult<Self> {
        if mode == AuditKeyMode::ExternalConsumer {
            return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
        }
        let path = root.join(".audit-key");
        super::files::reject_symlink_components(root)?;
        if path.exists() {
            super::files::reject_symlink(&path)?;
        }
        let keys = Arc::new(AuditKeys::load_or_create_file(&path)?);
        let mut keyring = vec![Arc::clone(&keys)];
        let epochs = root.join(".audit-key-epochs");
        if epochs.exists() {
            super::files::reject_symlink(&epochs)?;
            let mut paths: Vec<PathBuf> = std::fs::read_dir(&epochs)
                .map_err(|error| AuditError::Io(format!("audit key epochs: {error}")))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<_, _>>()
                .map_err(|error| AuditError::Io(format!("audit key epoch entry: {error}")))?;
            paths.sort();
            for epoch_path in paths {
                super::files::reject_symlink(&epoch_path)?;
                let name = epoch_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(AuditError::Poisoned(PoisonReason::KeyUnavailable))?;
                let epoch = name
                    .strip_prefix("epoch-")
                    .and_then(|name| name.strip_suffix(".key"))
                    .and_then(|name| name.parse::<u32>().ok())
                    .ok_or(AuditError::Poisoned(PoisonReason::KeyUnavailable))?;
                if epoch <= 1 {
                    return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
                }
                let epoch_keys = AuditKeys::load_epoch_file(&epoch_path, epoch)?;
                if epoch_keys.installation_id() != keys.installation_id() {
                    return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
                }
                keyring.push(Arc::new(epoch_keys));
            }
        }
        Ok(Self {
            mode,
            provider: Arc::new(FileAuditKeyProvider {
                root: root.to_path_buf(),
                root_key: Arc::clone(&keys),
                keys: parking_lot::Mutex::new(keyring),
            }),
        })
    }
}

#[derive(Debug)]
struct FileAuditKeyProvider {
    root: std::path::PathBuf,
    root_key: Arc<AuditKeys>,
    keys: parking_lot::Mutex<Vec<Arc<AuditKeys>>>,
}

impl AuditKeyProvider for FileAuditKeyProvider {
    fn keyring(&self) -> Vec<Arc<AuditKeys>> {
        self.keys.lock().clone()
    }

    fn rotate(&self, current: &AuditKeys) -> AuditResult<Arc<AuditKeys>> {
        let epoch = current
            .key_epoch()
            .checked_add(1)
            .ok_or(AuditError::Poisoned(PoisonReason::SequenceExhausted))?;
        let next = Arc::new(AuditKeys::derive_for_epoch(
            &self.root_key.root_material,
            epoch,
        ));
        next.persist_epoch(&self.root)?;
        let mut keys = self.keys.lock();
        if !keys.iter().any(|key| key.key_id() == next.key_id()) {
            keys.push(Arc::clone(&next));
        }
        Ok(next)
    }
}

#[derive(Debug)]
pub(crate) struct StaticAuditKeyProvider {
    keys: Vec<Arc<AuditKeys>>,
}

impl StaticAuditKeyProvider {
    pub(crate) fn new(keys: Vec<Arc<AuditKeys>>) -> Self {
        Self { keys }
    }
}

impl AuditKeyProvider for StaticAuditKeyProvider {
    fn keyring(&self) -> Vec<Arc<AuditKeys>> {
        self.keys.clone()
    }

    fn rotate(&self, _current: &AuditKeys) -> AuditResult<Arc<AuditKeys>> {
        Err(AuditError::Poisoned(PoisonReason::KeyUnavailable))
    }
}

impl std::fmt::Debug for AuditKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material, not even truncated.
        f.debug_struct("AuditKeys")
            .field("keyId", &self.key_id)
            .field("installationId", &self.installation_id)
            .finish()
    }
}

impl AuditKeys {
    pub fn derive(installation_key: &[u8]) -> Self {
        Self::derive_for_epoch(installation_key, 1)
    }

    pub(crate) fn derive_for_epoch(installation_key: &[u8], epoch: u32) -> Self {
        let epoch_label = |label: &[u8]| {
            if epoch == 1 {
                label.to_vec()
            } else {
                let mut value = label.to_vec();
                value.extend_from_slice(b"/epoch/");
                value.extend_from_slice(epoch.to_string().as_bytes());
                value
            }
        };
        let key_id_label = epoch_label(LABEL_KEY_ID);
        let key_id = hex32(&hmac_sha256(installation_key, &key_id_label))[..16].to_string();
        let installation_id =
            hex32(&hmac_sha256(installation_key, LABEL_INSTALL_ID))[..32].to_string();
        Self {
            root_material: installation_key.to_vec(),
            chain: hmac_sha256(installation_key, &epoch_label(LABEL_CHAIN)),
            manifest: hmac_sha256(installation_key, &epoch_label(LABEL_MANIFEST)),
            anchor: hmac_sha256(installation_key, &epoch_label(LABEL_ANCHOR)),
            seal: hmac_sha256(installation_key, &epoch_label(LABEL_SEAL)),
            actor: hmac_sha256(installation_key, &epoch_label(LABEL_ACTOR)),
            key_id,
            installation_id,
            epoch,
        }
    }

    pub(crate) fn persist_epoch(&self, root: &Path) -> AuditResult<()> {
        let dir = root.join(".audit-key-epochs");
        super::files::create_private_dir_all(&dir)?;
        let path = dir.join(format!("epoch-{:08}.key", self.epoch));
        if path.exists() {
            let existing = Self::load_epoch_file(&path, self.epoch)?;
            if existing.key_id() != self.key_id() {
                return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
            }
            return Ok(());
        }
        let bundle = EpochKeyBundle {
            epoch: self.epoch,
            key_id: self.key_id.clone(),
            installation_id: self.installation_id.clone(),
            chain: hex32(&self.chain),
            manifest: hex32(&self.manifest),
            anchor: hex32(&self.anchor),
            seal: hex32(&self.seal),
            actor: hex32(&self.actor),
        };
        write_private_epoch(&path, &bundle)
    }

    fn load_epoch_file(path: &Path, epoch: u32) -> AuditResult<Self> {
        let bundle = read_private_epoch(path)?;
        if bundle.epoch != epoch || bundle.key_id.is_empty() || bundle.installation_id.is_empty() {
            return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
        }
        Ok(Self {
            root_material: Vec::new(),
            chain: decode_key_component(&bundle.chain)?,
            manifest: decode_key_component(&bundle.manifest)?,
            anchor: decode_key_component(&bundle.anchor)?,
            seal: decode_key_component(&bundle.seal)?,
            actor: decode_key_component(&bundle.actor)?,
            key_id: bundle.key_id,
            installation_id: bundle.installation_id,
            epoch,
        })
    }

    pub(crate) fn key_epoch(&self) -> u32 {
        self.epoch
    }

    /// Load a private installation key from `path`, creating it on first use.
    ///
    /// The private-file predicate matches `live_attestation.rs`: a regular
    /// file, owned by the current user, mode `0600`, with exactly one link.
    /// Anything else fails closed rather than being repaired.
    pub fn load_or_create_file(path: &Path) -> AuditResult<Self> {
        if path.exists() {
            let material = read_private_key(path)?;
            return Ok(Self::derive(&material));
        }
        let mut material = [0u8; 32];
        material[..16].copy_from_slice(Uuid::new_v4().as_bytes());
        material[16..].copy_from_slice(Uuid::new_v4().as_bytes());
        write_private_key(path, &material)?;
        Ok(Self::derive(&material))
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn installation_id(&self) -> &str {
        &self.installation_id
    }

    pub(crate) fn genesis_tag(&self) -> String {
        hex32(&hmac_sha256(&self.chain, LABEL_GENESIS))
    }

    pub(crate) fn chain_tag(&self, previous: &str, payload: &[u8]) -> String {
        let mut input = Vec::with_capacity(previous.len() + payload.len());
        input.extend_from_slice(previous.as_bytes());
        input.extend_from_slice(payload);
        hex32(&hmac_sha256(&self.chain, &input))
    }

    /// Authenticated boundary tag for imported legacy bytes.
    ///
    /// This attests *which exact bytes were imported*, not that their contents
    /// are tamper-evident: the legacy v1 ledger had no chain to preserve.
    pub(crate) fn import_seal_tag(&self, generation_id: &str, journal_sha256: &str) -> String {
        let mut input = Vec::new();
        input.extend_from_slice(LABEL_IMPORT_SEAL);
        input.push(0);
        input.extend_from_slice(generation_id.as_bytes());
        input.push(0);
        input.extend_from_slice(journal_sha256.as_bytes());
        hex32(&hmac_sha256(&self.chain, &input))
    }

    pub(crate) fn manifest_mac(&self, payload: &[u8]) -> String {
        hex32(&hmac_sha256(&self.manifest, payload))
    }

    pub(crate) fn anchor_mac(&self, payload: &[u8]) -> String {
        hex32(&hmac_sha256(&self.anchor, payload))
    }

    pub(crate) fn seal_mac(&self, payload: &[u8]) -> String {
        hex32(&hmac_sha256(&self.seal, payload))
    }

    /// Keyed, truncated digest for an actor / request / scope identifier.
    ///
    /// Correlatable inside one installation, meaningless outside it. This is
    /// what keeps raw ids, paths and request identifiers out of the journal.
    pub(crate) fn opaque_digest(&self, value: &str) -> String {
        hex32(&hmac_sha256(&self.actor, value.as_bytes()))[..32].to_string()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EpochKeyBundle {
    epoch: u32,
    key_id: String,
    installation_id: String,
    chain: String,
    manifest: String,
    anchor: String,
    seal: String,
    actor: String,
}

fn decode_key_component(value: &str) -> AuditResult<[u8; 32]> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
    }
    let mut decoded = [0u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = (pair[0] as char)
            .to_digit(16)
            .ok_or(AuditError::Poisoned(PoisonReason::KeyUnavailable))?;
        let low = (pair[1] as char)
            .to_digit(16)
            .ok_or(AuditError::Poisoned(PoisonReason::KeyUnavailable))?;
        decoded[index] = ((high << 4) | low) as u8;
    }
    Ok(decoded)
}

fn write_private_epoch(path: &Path, bundle: &EpochKeyBundle) -> AuditResult<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| AuditError::Io(format!("audit epoch create: {error}")))?;
    let bytes = serde_json::to_vec(bundle)
        .map_err(|error| AuditError::Io(format!("audit epoch serialize: {error}")))?;
    file.write_all(&bytes)
        .map_err(|error| AuditError::Io(format!("audit epoch write: {error}")))?;
    file.sync_all()
        .map_err(|error| AuditError::Io(format!("audit epoch sync: {error}")))?;
    if let Some(parent) = path.parent() {
        super::files::fsync_dir(parent)?;
    }
    Ok(())
}

fn read_private_epoch(path: &Path) -> AuditResult<EpochKeyBundle> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| AuditError::Io(format!("audit epoch metadata: {error}")))?;
    if !metadata.is_file() {
        return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current_uid = unsafe { libc::getuid() };
        if metadata.uid() != current_uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
        }
    }
    let bytes = std::fs::read(path)
        .map_err(|error| AuditError::Io(format!("audit epoch read: {error}")))?;
    serde_json::from_slice(&bytes).map_err(|_| AuditError::Poisoned(PoisonReason::KeyUnavailable))
}

fn read_private_key(path: &Path) -> AuditResult<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| AuditError::Io(format!("audit key metadata: {error}")))?;
    if !metadata.is_file() {
        return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let current_uid = unsafe { libc::getuid() };
        if metadata.uid() != current_uid
            || metadata.mode() & 0o777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
        }
    }
    let text = std::fs::read_to_string(path)
        .map_err(|error| AuditError::Io(format!("audit key read: {error}")))?;
    let trimmed = text.trim();
    if trimmed.len() != 64 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
    }
    let mut material = Vec::with_capacity(32);
    let bytes = trimmed.as_bytes();
    for pair in bytes.chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
        let lo = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
        material.push((hi << 4) | lo);
    }
    Ok(material)
}

fn write_private_key(path: &Path, material: &[u8]) -> AuditResult<()> {
    if material.len() != 32 {
        return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
    }
    use std::io::Write;

    if let Some(parent) = path.parent() {
        super::files::create_private_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|error| AuditError::Io(format!("audit key create: {error}")))?;
    let mut encoded = String::with_capacity(65);
    for byte in material {
        encoded.push_str(&format!("{byte:02x}"));
    }
    encoded.push('\n');
    file.write_all(encoded.as_bytes())
        .map_err(|error| AuditError::Io(format!("audit key write: {error}")))?;
    file.sync_all()
        .map_err(|error| AuditError::Io(format!("audit key sync: {error}")))?;
    Ok(())
}
