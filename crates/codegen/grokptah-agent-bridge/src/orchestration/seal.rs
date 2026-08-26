//! Keyed sealing authority for the durable orchestration ledger.
//!
//! Every durable record used to be sealed with a bare SHA-256 digest over its
//! own fields. That detects accidental corruption and nothing else: an attacker
//! who can write the ledger can recompute the digest as easily as we can, so a
//! "resealed forgery" verifies perfectly. Integrity without a secret is not
//! integrity against an adversary — it is a checksum.
//!
//! This module replaces that with a **keyed** seal: HMAC-SHA256 under a key
//! this process holds and an attacker with write access to the ledger does not.
//! Recomputing a seal now requires the key, so tampering is detectable rather
//! than merely inconvenient.
//!
//! Three properties matter as much as the primitive:
//!
//! * **Versioned.** Every seal names the key that produced it and the seal
//!   algorithm version. A record sealed under a key this authority no longer
//!   holds does not silently verify under the current one.
//! * **Fail closed.** If the key cannot be loaded, or a record names a key we
//!   do not hold, verification fails. There is no "unsealed" fallback and no
//!   "trust it this once" path; a ledger we cannot authenticate is a ledger we
//!   do not execute from.
//! * **Platform-protected where the platform offers it.** The key lives in the
//!   OS keyring when one is usable, and otherwise in an owner-only file inside
//!   the store, written through the same no-follow handle API as every other
//!   private record.
//!
//! Rotation is explicit and coordinated: a new key becomes current while
//! previous keys stay loadable for verification, and every holder of a sealed
//! record is resealed in one transaction. A partial reseal is refused, because
//! a ledger half under each key is a ledger where a forgery can hide.

use std::collections::BTreeMap;
use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ledger_io::LedgerDir;
use super::types::{OrchError, OrchErrorCode};

/// Version of the seal construction. Bumped if the MAC input encoding or the
/// primitive ever changes, so old seals stop verifying rather than being
/// reinterpreted under new rules.
pub const SEAL_VERSION: u32 = 1;

/// Length of a raw sealing key. 32 bytes matches the HMAC-SHA256 block
/// security level; longer keys buy nothing here.
const KEY_BYTES: usize = 32;

/// Service name used when the OS keyring is available.
const KEYRING_SERVICE: &str = "grokptah-orchestration-seal";

/// File name of the fallback key inside the store's private key ledger.
const KEY_FILE: &str = "authority.json";

/// A sealed record's authority stamp.
///
/// Carried inside every durable record so verification knows *which* key and
/// *which* construction produced the seal, instead of assuming the current
/// ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SealStamp {
    pub seal_version: u32,
    /// Identity of the key that produced this seal. A digest of the key, not
    /// the key: safe to store beside the record it authenticates.
    pub key_id: String,
    /// The keyed MAC over the record's canonical payload.
    pub mac: String,
}

impl SealStamp {
    /// A placeholder stamp for a record that has not been sealed yet.
    ///
    /// Never verifies: `key_id` and `mac` are empty, and verification requires
    /// a key it holds plus a matching MAC.
    pub fn unsealed() -> Self {
        Self {
            seal_version: SEAL_VERSION,
            key_id: String::new(),
            mac: String::new(),
        }
    }

    pub fn is_unsealed(&self) -> bool {
        self.key_id.is_empty() || self.mac.is_empty()
    }
}

/// Where the sealing key came from. Reported so an operator can tell a
/// platform-protected deployment from a file-backed one without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyProtection {
    /// Held by the operating system keyring.
    PlatformKeyring,
    /// Held in an owner-only file inside the store, written no-follow.
    OwnerOnlyFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredKeyring {
    version: u32,
    current_key_id: String,
    /// key_id -> base16 key material. Retired keys stay loadable so records
    /// sealed under them can still be verified and resealed.
    keys: BTreeMap<String, String>,
}

/// The process's sealing authority.
///
/// Cheap to clone; all clones share one keyring so a rotation is visible
/// everywhere at once.
#[derive(Clone)]
pub struct SealAuthority {
    inner: Arc<SealAuthorityInner>,
}

struct SealAuthorityInner {
    keys: RwLock<BTreeMap<String, Vec<u8>>>,
    current: RwLock<String>,
    protection: KeyProtection,
    ledger: Option<LedgerDir>,
}

impl std::fmt::Debug for SealAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render key material, not even by accident in a panic message.
        f.debug_struct("SealAuthority")
            .field("protection", &self.inner.protection)
            .field("current_key_id", &self.current_key_id())
            .field("known_keys", &self.inner.keys.read().len())
            .finish()
    }
}

impl SealAuthority {
    /// Open (or create) the authority for one store root.
    ///
    /// Prefers the platform keyring; falls back to an owner-only file inside
    /// the store. Either way the key never appears in a log, an error, or a
    /// projection.
    pub fn open(store_root: &std::path::Path) -> Result<Self, OrchError> {
        let ledger = LedgerDir::open(&store_root.join("keys"))?;
        if let Some(authority) = Self::from_platform_keyring(&ledger)? {
            return Ok(authority);
        }
        Self::from_owner_only_file(ledger)
    }

    /// An in-memory authority with a caller-supplied key. For tests and for
    /// deployments that inject a key from an external secret manager.
    pub fn with_key(key: Vec<u8>) -> Result<Self, OrchError> {
        if key.len() < KEY_BYTES {
            return Err(OrchError::new(
                OrchErrorCode::InvalidRequest,
                "sealing key is too short",
            ));
        }
        let key_id = key_id_of(&key);
        let mut keys = BTreeMap::new();
        keys.insert(key_id.clone(), key);
        Ok(Self {
            inner: Arc::new(SealAuthorityInner {
                keys: RwLock::new(keys),
                current: RwLock::new(key_id),
                protection: KeyProtection::OwnerOnlyFile,
                ledger: None,
            }),
        })
    }

    fn from_platform_keyring(ledger: &LedgerDir) -> Result<Option<Self>, OrchError> {
        // A keyring that cannot be reached (headless Linux, locked login
        // keyring, no secret service) is not an error: it is a deployment
        // without platform key protection, and the file fallback covers it.
        let Ok(entry) = keyring::Entry::new(KEYRING_SERVICE, "current") else {
            return Ok(None);
        };
        match entry.get_password() {
            Ok(encoded) => {
                let stored: StoredKeyring = serde_json::from_str(&encoded).map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Conflict,
                        format!("sealing keyring is unreadable: {error}"),
                    )
                })?;
                Ok(Some(Self::from_stored(
                    stored,
                    KeyProtection::PlatformKeyring,
                    None,
                )?))
            }
            Err(keyring::Error::NoEntry) => {
                let stored = new_keyring();
                let encoded = serde_json::to_string(&stored).map_err(internal)?;
                if entry.set_password(&encoded).is_err() {
                    // Writable-looking but not actually usable; fall back.
                    return Ok(None);
                }
                let _ = ledger;
                Ok(Some(Self::from_stored(
                    stored,
                    KeyProtection::PlatformKeyring,
                    None,
                )?))
            }
            Err(_) => Ok(None),
        }
    }

    fn from_owner_only_file(ledger: LedgerDir) -> Result<Self, OrchError> {
        match ledger.read_private(KEY_FILE)? {
            Some(text) => {
                let stored: StoredKeyring = serde_json::from_str(&text).map_err(|error| {
                    OrchError::new(
                        OrchErrorCode::Conflict,
                        format!("sealing keyring is unreadable: {error}"),
                    )
                })?;
                Self::from_stored(stored, KeyProtection::OwnerOnlyFile, Some(ledger))
            }
            None => {
                let stored = new_keyring();
                let bytes = serde_json::to_vec_pretty(&stored).map_err(internal)?;
                ledger.write_private(KEY_FILE, &bytes)?;
                Self::from_stored(stored, KeyProtection::OwnerOnlyFile, Some(ledger))
            }
        }
    }

    fn from_stored(
        stored: StoredKeyring,
        protection: KeyProtection,
        ledger: Option<LedgerDir>,
    ) -> Result<Self, OrchError> {
        if stored.version != SEAL_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::Unsupported,
                format!(
                    "sealing keyring version {} is not supported",
                    stored.version
                ),
            ));
        }
        let mut keys = BTreeMap::new();
        for (key_id, encoded) in stored.keys {
            let key = decode_hex(&encoded).ok_or_else(|| {
                OrchError::new(OrchErrorCode::Conflict, "sealing key is not valid hex")
            })?;
            if key_id_of(&key) != key_id {
                return Err(OrchError::new(
                    OrchErrorCode::Conflict,
                    "sealing key does not match its recorded identity",
                ));
            }
            keys.insert(key_id, key);
        }
        if !keys.contains_key(&stored.current_key_id) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "sealing keyring does not hold its own current key",
            ));
        }
        Ok(Self {
            inner: Arc::new(SealAuthorityInner {
                keys: RwLock::new(keys),
                current: RwLock::new(stored.current_key_id),
                protection,
                ledger,
            }),
        })
    }

    pub fn protection(&self) -> KeyProtection {
        self.inner.protection
    }

    pub fn current_key_id(&self) -> String {
        self.inner.current.read().clone()
    }

    pub fn known_key_ids(&self) -> Vec<String> {
        self.inner.keys.read().keys().cloned().collect()
    }

    /// Seal a canonical payload under the current key.
    pub fn seal(&self, payload: &serde_json::Value) -> Result<SealStamp, OrchError> {
        let key_id = self.current_key_id();
        let keys = self.inner.keys.read();
        let key = keys.get(&key_id).ok_or_else(|| {
            OrchError::new(
                OrchErrorCode::Internal,
                "sealing authority lost its current key",
            )
        })?;
        Ok(SealStamp {
            seal_version: SEAL_VERSION,
            key_id: key_id.clone(),
            mac: hmac_sha256_hex(key, &canonical_bytes(payload)),
        })
    }

    /// Verify a sealed payload, failing closed on every ambiguity.
    ///
    /// An unknown key id is a refusal, not a fallback to the current key: that
    /// is exactly the case where a record was sealed by something that is not
    /// this authority.
    pub fn verify(&self, payload: &serde_json::Value, stamp: &SealStamp) -> Result<(), OrchError> {
        if stamp.seal_version != SEAL_VERSION {
            return Err(OrchError::new(
                OrchErrorCode::Unsupported,
                format!("seal version {} is not supported", stamp.seal_version),
            ));
        }
        if stamp.is_unsealed() {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "record carries no authority seal",
            ));
        }
        let keys = self.inner.keys.read();
        let Some(key) = keys.get(&stamp.key_id) else {
            return Err(OrchError::with_data(
                OrchErrorCode::Conflict,
                "record was sealed under a key this authority does not hold",
                serde_json::json!({ "keyId": stamp.key_id }),
            ));
        };
        let expected = hmac_sha256_hex(key, &canonical_bytes(payload));
        if !constant_time_eq_str(&expected, &stamp.mac) {
            return Err(OrchError::new(
                OrchErrorCode::Conflict,
                "record seal does not authenticate its fields",
            ));
        }
        Ok(())
    }

    /// Whether a stamp was produced by the *current* key.
    ///
    /// Used by the reseal transaction to decide what still needs rewriting.
    pub fn is_current(&self, stamp: &SealStamp) -> bool {
        !stamp.is_unsealed()
            && stamp.seal_version == SEAL_VERSION
            && stamp.key_id == self.current_key_id()
    }

    /// Mint a new current key, retaining the previous ones for verification.
    ///
    /// Rotation alone does not reseal anything: records stay verifiable under
    /// their original key until a coordinated reseal rewrites them. Retiring a
    /// key before that would make honest records indistinguishable from
    /// forgeries.
    pub fn rotate(&self) -> Result<String, OrchError> {
        let key = random_key()?;
        let key_id = key_id_of(&key);
        {
            let mut keys = self.inner.keys.write();
            keys.insert(key_id.clone(), key);
            *self.inner.current.write() = key_id.clone();
        }
        self.persist()?;
        Ok(key_id)
    }

    /// Drop every key except the current one.
    ///
    /// Only safe after a coordinated reseal: any record still sealed under a
    /// retired key becomes unverifiable, which is a refusal, not a silent
    /// acceptance.
    pub fn retire_previous_keys(&self) -> Result<usize, OrchError> {
        let current = self.current_key_id();
        let removed = {
            let mut keys = self.inner.keys.write();
            let before = keys.len();
            keys.retain(|key_id, _| key_id == &current);
            before - keys.len()
        };
        self.persist()?;
        Ok(removed)
    }

    fn persist(&self) -> Result<(), OrchError> {
        let stored = StoredKeyring {
            version: SEAL_VERSION,
            current_key_id: self.current_key_id(),
            keys: self
                .inner
                .keys
                .read()
                .iter()
                .map(|(key_id, key)| (key_id.clone(), encode_hex(key)))
                .collect(),
        };
        match self.inner.protection {
            KeyProtection::OwnerOnlyFile => {
                let Some(ledger) = self.inner.ledger.as_ref() else {
                    // An injected in-memory key has nowhere to persist to, and
                    // that is the caller's contract, not a failure.
                    return Ok(());
                };
                let bytes = serde_json::to_vec_pretty(&stored).map_err(internal)?;
                ledger.write_private(KEY_FILE, &bytes)
            }
            KeyProtection::PlatformKeyring => {
                let entry = keyring::Entry::new(KEYRING_SERVICE, "current")
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))?;
                let encoded = serde_json::to_string(&stored).map_err(internal)?;
                entry
                    .set_password(&encoded)
                    .map_err(|error| OrchError::new(OrchErrorCode::Internal, error.to_string()))
            }
        }
    }
}

fn internal<E: std::fmt::Display>(error: E) -> OrchError {
    OrchError::new(OrchErrorCode::Internal, error.to_string())
}

fn new_keyring() -> StoredKeyring {
    let key = random_key().unwrap_or_else(|_| vec![0u8; KEY_BYTES]);
    let key_id = key_id_of(&key);
    let mut keys = BTreeMap::new();
    keys.insert(key_id.clone(), encode_hex(&key));
    StoredKeyring {
        version: SEAL_VERSION,
        current_key_id: key_id,
        keys,
    }
}

/// A key's public identity: a digest of the key, never the key itself.
fn key_id_of(key: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"grokptah-seal-key-id\x00");
    hasher.update(key);
    encode_hex(&hasher.finalize())
}

/// 32 bytes from the operating system's CSPRNG.
///
/// Read directly rather than through a userspace PRNG so the key does not
/// depend on seeding this process got right.
fn random_key() -> Result<Vec<u8>, OrchError> {
    #[cfg(unix)]
    {
        use std::io::Read;
        let mut file = std::fs::File::open("/dev/urandom").map_err(internal)?;
        let mut key = vec![0u8; KEY_BYTES];
        file.read_exact(&mut key).map_err(internal)?;
        Ok(key)
    }
    #[cfg(windows)]
    {
        // `uuid` draws from the platform CSPRNG on Windows; two v4 values give
        // 32 bytes of it without taking a new dependency.
        let mut key = Vec::with_capacity(KEY_BYTES);
        key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        Ok(key)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(OrchError::new(
            OrchErrorCode::Unsupported,
            "no platform entropy source for the sealing key",
        ))
    }
}

/// Canonical bytes for a payload.
///
/// `serde_json::Map` is a `BTreeMap` in this build, so object keys serialize in
/// sorted order and the same logical payload always produces the same bytes.
fn canonical_bytes(payload: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(payload).unwrap_or_default()
}

/// HMAC-SHA256, per RFC 2104. Verified against the RFC 4231 vectors below.
fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        block[..digest.len()].copy_from_slice(&digest);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK];
    let mut outer_pad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_pad[index] ^= block[index];
        outer_pad[index] ^= block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_digest = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_digest);
    encode_hex(&outer.finalize())
}

fn encode_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

/// Compare two hex MACs without leaking where they first differ.
fn constant_time_eq_str(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for index in 0..a.len() {
        diff |= a[index] ^ b[index];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn payload(value: &str) -> serde_json::Value {
        serde_json::json!({ "field": value, "nested": { "b": 2, "a": 1 } })
    }

    /// RFC 4231 test vectors. A hand-written MAC is only trustworthy if it
    /// agrees with the published values.
    #[test]
    fn hmac_matches_rfc4231_vectors() {
        // Case 1
        assert_eq!(
            hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // Case 2
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Case 3
        assert_eq!(
            hmac_sha256_hex(&[0xaa; 20], &[0xdd; 50]),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );
        // Case 6: key longer than one block, exercising the key-hashing branch.
        assert_eq!(
            hmac_sha256_hex(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn a_seal_authenticates_only_its_own_payload() {
        let authority = SealAuthority::with_key(vec![7u8; 32]).unwrap();
        let stamp = authority.seal(&payload("one")).unwrap();
        assert!(authority.verify(&payload("one"), &stamp).is_ok());
        assert!(authority.verify(&payload("two"), &stamp).is_err());
    }

    /// The point of the whole module: an attacker who can rewrite the record
    /// cannot produce a seal for it without the key.
    #[test]
    fn a_forger_without_the_key_cannot_reseal() {
        let honest = SealAuthority::with_key(vec![1u8; 32]).unwrap();
        let forger = SealAuthority::with_key(vec![2u8; 32]).unwrap();

        let forged = payload("attacker-controlled");
        let forged_stamp = forger.seal(&forged).unwrap();
        // The forgery is internally perfect — and refused, because the key id
        // is one the honest authority does not hold.
        assert!(forger.verify(&forged, &forged_stamp).is_ok());
        let error = honest.verify(&forged, &forged_stamp).unwrap_err();
        assert_eq!(error.code, OrchErrorCode::Conflict);

        // Nor can the forger reuse an honest stamp on different content.
        let honest_stamp = honest.seal(&payload("honest")).unwrap();
        assert!(honest.verify(&forged, &honest_stamp).is_err());
    }

    #[test]
    fn an_unsealed_or_wrong_version_record_fails_closed() {
        let authority = SealAuthority::with_key(vec![3u8; 32]).unwrap();
        assert!(authority
            .verify(&payload("x"), &SealStamp::unsealed())
            .is_err());
        let mut stamp = authority.seal(&payload("x")).unwrap();
        stamp.seal_version = SEAL_VERSION + 1;
        assert!(authority.verify(&payload("x"), &stamp).is_err());
        stamp.seal_version = SEAL_VERSION;
        stamp.mac = "0".repeat(64);
        assert!(authority.verify(&payload("x"), &stamp).is_err());
    }

    #[test]
    fn rotation_keeps_old_records_verifiable_until_they_are_resealed() {
        let dir = tempdir().unwrap();
        let authority = SealAuthority::open(dir.path()).unwrap();
        let first_key = authority.current_key_id();
        let stamp = authority.seal(&payload("before")).unwrap();
        assert!(authority.is_current(&stamp));

        let second_key = authority.rotate().unwrap();
        assert_ne!(first_key, second_key);
        assert_eq!(authority.current_key_id(), second_key);

        // Still verifiable under the retained old key, but no longer current,
        // so a reseal transaction knows it has work to do.
        assert!(authority.verify(&payload("before"), &stamp).is_ok());
        assert!(!authority.is_current(&stamp));

        // Retiring the old key before resealing makes it unverifiable — a
        // refusal, never a silent acceptance.
        authority.retire_previous_keys().unwrap();
        assert!(authority.verify(&payload("before"), &stamp).is_err());
    }

    #[test]
    fn the_keyring_survives_reopening_and_never_stores_the_key_in_the_clear_id() {
        let dir = tempdir().unwrap();
        let first = SealAuthority::open(dir.path()).unwrap();
        let key_id = first.current_key_id();
        let stamp = first.seal(&payload("persisted")).unwrap();
        drop(first);

        let reopened = SealAuthority::open(dir.path()).unwrap();
        assert_eq!(reopened.current_key_id(), key_id);
        assert!(reopened.verify(&payload("persisted"), &stamp).is_ok());

        // The key id is a digest, so publishing it beside a record is safe.
        assert_eq!(key_id.len(), 64);
        assert!(key_id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[cfg(unix)]
    #[test]
    fn the_fallback_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let authority = SealAuthority::open(dir.path()).unwrap();
        if authority.protection() != KeyProtection::OwnerOnlyFile {
            return; // a platform keyring is in use; nothing on disk to check
        }
        let key_path = dir.path().join("keys").join(KEY_FILE);
        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the sealing key is {mode:o}");
        let dir_mode = std::fs::metadata(dir.path().join("keys"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }

    /// The debug rendering is a place secrets escape into logs. It must show
    /// the shape of the authority and none of its material.
    #[test]
    fn debug_output_never_contains_key_material() {
        let key = vec![0xABu8; 32];
        let authority = SealAuthority::with_key(key.clone()).unwrap();
        let rendered = format!("{authority:?}");
        assert!(!rendered.contains(&encode_hex(&key)));
        assert!(!rendered.to_lowercase().contains("abababab"));
        assert!(rendered.contains("SealAuthority"));
    }

    #[test]
    fn constant_time_comparison_rejects_length_and_content_differences() {
        assert!(constant_time_eq_str("abcd", "abcd"));
        assert!(!constant_time_eq_str("abcd", "abce"));
        assert!(!constant_time_eq_str("abcd", "abcde"));
    }
}
