//! Audit chain keys: HMAC-SHA256 and domain-separated subkey derivation (#443).
//!
//! HMAC is implemented over the crate's existing `sha2` dependency rather than
//! pulling in the `hmac` crate, so the audit authority adds zero entries to
//! `crates/codegen/grokptah-agent-bridge/Cargo.lock` and `--locked` checks stay
//! reproducible offline.

use std::path::{Path, PathBuf};

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
const LABEL_AUTHORITY: &[u8] = b"grokptah-audit.v2/authority";

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
pub struct AuditKeys {
    chain: [u8; 32],
    manifest: [u8; 32],
    anchor: [u8; 32],
    seal: [u8; 32],
    actor: [u8; 32],
    authority: [u8; 32],
    key_id: String,
    installation_id: String,
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
        let key_id = hex32(&hmac_sha256(installation_key, LABEL_KEY_ID))[..16].to_string();
        let installation_id =
            hex32(&hmac_sha256(installation_key, LABEL_INSTALL_ID))[..32].to_string();
        Self {
            chain: hmac_sha256(installation_key, LABEL_CHAIN),
            manifest: hmac_sha256(installation_key, LABEL_MANIFEST),
            anchor: hmac_sha256(installation_key, LABEL_ANCHOR),
            seal: hmac_sha256(installation_key, LABEL_SEAL),
            actor: hmac_sha256(installation_key, LABEL_ACTOR),
            authority: hmac_sha256(installation_key, LABEL_AUTHORITY),
            key_id,
            installation_id,
        }
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

    /// Tag over a capability grant.
    ///
    /// Its own subkey: a grant must not be verifiable by anything that can
    /// produce a seal, an anchor or a chain tag, so a captured tag from one
    /// document class can never be replayed as authority for another.
    pub(crate) fn authority_mac(&self, payload: &[u8]) -> String {
        hex32(&hmac_sha256(&self.authority, payload))
    }

    /// Keyed, truncated digest for an actor / request / scope identifier.
    ///
    /// Correlatable inside one installation, meaningless outside it. This is
    /// what keeps raw ids, paths and request identifiers out of the journal.
    pub(crate) fn opaque_digest(&self, value: &str) -> String {
        hex32(&hmac_sha256(&self.actor, value.as_bytes()))[..32].to_string()
    }
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

fn write_private_key(path: &Path, material: &[u8; 32]) -> AuditResult<()> {
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

/// Where the installation key comes from, per deployment mode (#462).
///
/// Every variant fails closed rather than silently degrading to an
/// unauthenticated ledger: an audit trail nobody can verify is worse than one
/// that refuses to open, because only the second is honest about it.
#[derive(Debug, Clone)]
pub enum AuditKeyCustody {
    /// Packaged desktop. The OS keychain holds the key; the caller supplies it
    /// because `keyring` access belongs to the shell, not to this library.
    /// Absent material fails closed.
    Provided(Vec<u8>),
    /// Headless service. 64 hex characters in the named environment variable.
    /// Absent or malformed fails closed — a service that was configured for a
    /// managed key must not quietly invent one.
    Environment { var: String },
    /// Local file, created on first use with mode `0600`. Used by the packaged
    /// desktop's fallback and by tests. Unsafe ownership, mode, or link count
    /// fails closed; it is never repaired.
    LocalFile { path: PathBuf },
}

impl AuditKeyCustody {
    /// Default for a store root: a private key file beside the ledger.
    pub fn local_file_for(root: &Path) -> Self {
        Self::LocalFile {
            path: root.join("audit.key"),
        }
    }

    /// Resolve to derived keys.
    ///
    /// Errors deliberately carry no path and no key bytes: the caller may
    /// surface [`AuditError::code`] on a public health projection.
    pub fn resolve(&self) -> AuditResult<AuditKeys> {
        match self {
            Self::Provided(material) => {
                if material.len() < 16 {
                    return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
                }
                Ok(AuditKeys::derive(material))
            }
            Self::Environment { var } => {
                let value = std::env::var(var)
                    .map_err(|_| AuditError::Poisoned(PoisonReason::KeyUnavailable))?;
                let trimmed = value.trim();
                if trimmed.len() != 64 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(AuditError::Poisoned(PoisonReason::KeyUnavailable));
                }
                Ok(AuditKeys::derive(&decode_hex(trimmed)))
            }
            Self::LocalFile { path } => AuditKeys::load_or_create_file(path),
        }
    }
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16).unwrap_or(0) as u8;
            let lo = (pair[1] as char).to_digit(16).unwrap_or(0) as u8;
            (hi << 4) | lo
        })
        .collect()
}
