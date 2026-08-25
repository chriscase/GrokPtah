//! Authorization snapshots and opaque root tokens.
//!
//! # The single authority path
//!
//! Every read follows exactly one route:
//!
//! 1. A caller asks for a **snapshot**. Issuing one is non-mutating: it reads
//!    live authorization state, observes each candidate root's on-disk
//!    identity, and records the result in this in-process registry. Nothing
//!    durable changes, so a viewer that crashes mid-snapshot leaves no trace.
//! 2. The snapshot returns one **opaque token per root**. A token is the only
//!    way to name a root. There is no "the workspace", no ordinal, and no
//!    fallback: a caller that cannot produce a token gets a refusal, never a
//!    guess about which tree it meant.
//! 3. Every read presents a token **and** the acting authorization context.
//!    Both the principal and the policy fingerprint are recomputed at action
//!    time and compared against the snapshot, so a token cannot outlive the
//!    authorization that produced it or be replayed by another principal.
//!
//! # Replay policy
//!
//! A token is a bearer capability for one root in one snapshot. Reads are
//! non-mutating and idempotent, so replaying a token **within** its validity
//! window is permitted by design — that is what paging through a file is. A
//! replay is refused when any of the following holds, and each is tested:
//!
//! * the authentication tag does not verify (forged or tampered);
//! * the snapshot is unknown — swept, evicted, or never issued;
//! * the snapshot was explicitly revoked;
//! * the deadline has passed;
//! * the acting principal differs from the issued principal;
//! * the policy fingerprint differs from the issued fingerprint;
//! * the root's on-disk identity differs from the observed identity.
//!
//! # Cleanup
//!
//! Expired snapshots are swept on every issue and every resolve, and the
//! registry is capped: past the cap the lowest revisions are evicted. A
//! process that issues snapshots forever therefore holds bounded memory and a
//! bounded number of open directory handles, and an evicted token fails closed
//! as `snapshot_unknown`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::approval::short_root_label;
use crate::clock::Clock;
use crate::digest::{constant_time_eq, from_hex, tagged_mac, to_hex};
use crate::error::SourceViewError;
use crate::identity::path_digest;
use crate::open::RootHandle;
use crate::principal::AuthorizationContext;

/// Version prefix carried by every token. A token minted under a different
/// version is refused rather than reinterpreted.
pub const TOKEN_VERSION: &str = "sv1";

/// Machine-readable statement of the replay rule, asserted by contract tests
/// on both sides of the boundary.
pub const REPLAY_POLICY: &str = "idempotent-within-validity";

const DEFAULT_TTL_MS: u64 = 15 * 60 * 1000;
/// Live snapshots the registry keeps.
///
/// Each snapshot holds one open directory handle per approved root, so this
/// cap is a file-descriptor budget as much as a memory one: a few roots per
/// snapshot at this cap stays well inside the 256-descriptor soft limit macOS
/// still ships. Sweeping on every issue and resolve means a working session
/// rarely approaches it.
const DEFAULT_MAX_SNAPSHOTS: usize = 16;
const MAC_BYTES: usize = 16;

/// Whether a boundary is the shared workspace or one isolated run worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Workspace,
    IsolatedWorktree,
}

/// A directory the caller proposes for approval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRoot {
    pub kind: RootKind,
    pub path: PathBuf,
    pub run_id: Option<String>,
}

impl CandidateRoot {
    pub fn workspace(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: RootKind::Workspace,
            path: path.into(),
            run_id: None,
        }
    }

    pub fn worktree(path: impl Into<PathBuf>, run_id: impl Into<String>) -> Self {
        Self {
            kind: RootKind::IsolatedWorktree,
            path: path.into(),
            run_id: Some(run_id.into()),
        }
    }
}

/// The redacted, wire-safe description of one approved root.
///
/// There is no absolute path here by construction: a caller that wants to know
/// *which* tree it is looking at compares `pathDigest`, and a person reads
/// `label` plus the digest's short form.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootDescriptor {
    pub token: String,
    pub kind: RootKind,
    pub label: String,
    pub path_digest: String,
    pub identity_digest: String,
    pub run_id: Option<String>,
}

/// A non-mutating projection of everything one principal may inspect.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RootSnapshot {
    pub snapshot_id: String,
    pub revision: u64,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub principal_fingerprint: String,
    pub policy_fingerprint: String,
    pub replay_policy: &'static str,
    pub roots: Vec<RootDescriptor>,
}

/// A root that passed every action-time check.
#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub descriptor: RootDescriptor,
    /// The held-open root directory. Reads go through this, never through a
    /// path that could be re-pointed between here and the open.
    pub handle: RootHandle,
    pub snapshot_id: String,
    pub revision: u64,
}

#[derive(Debug, Clone)]
struct ApprovedRoot {
    descriptor: RootDescriptor,
    /// The root directory, held open for the life of the snapshot. This is
    /// what reads resolve against.
    handle: RootHandle,
}

#[derive(Debug)]
struct StoredSnapshot {
    snapshot: RootSnapshot,
    roots: Vec<ApprovedRoot>,
    principal_fingerprint: String,
    policy_fingerprint: String,
    expires_at_ms: u64,
    revoked: bool,
}

#[derive(Debug, Default)]
struct StoreState {
    revision: u64,
    entries: BTreeMap<String, StoredSnapshot>,
}

/// The registry of live authorization snapshots.
#[derive(Debug)]
pub struct SnapshotStore {
    key: [u8; 32],
    ttl_ms: u64,
    max_snapshots: usize,
    clock: Arc<dyn Clock>,
    state: Mutex<StoreState>,
}

impl SnapshotStore {
    /// Build a store. `key` must be process-unique secret entropy: it is the
    /// only thing standing between a caller and a forged token.
    pub fn new(key: [u8; 32], clock: Arc<dyn Clock>) -> Self {
        Self {
            key,
            ttl_ms: DEFAULT_TTL_MS,
            max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            clock,
            state: Mutex::new(StoreState::default()),
        }
    }

    pub fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = ttl_ms.max(1);
        self
    }

    pub fn with_capacity(mut self, max_snapshots: usize) -> Self {
        self.max_snapshots = max_snapshots.max(1);
        self
    }

    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StoreState> {
        // A poisoned registry is still a valid registry: entries are plain
        // data and every read re-validates. Recovering beats refusing all
        // inspection because one unrelated thread panicked.
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Issue a snapshot for `context` over `candidates`.
    ///
    /// Candidates whose directory cannot be observed are dropped rather than
    /// failing the whole snapshot: one stale worktree must not make the
    /// workspace uninspectable. A snapshot with no roots is still issued, so
    /// the caller learns "nothing is approved" as an answer rather than an
    /// error.
    pub fn issue(
        &self,
        context: &AuthorizationContext,
        candidates: &[CandidateRoot],
    ) -> RootSnapshot {
        let now = self.clock.now_ms();
        let mut state = self.lock();
        sweep_locked(&mut state, now);

        state.revision = state.revision.saturating_add(1);
        let revision = state.revision;
        let principal_fingerprint = context.principal.fingerprint();
        let policy_fingerprint = context.policy.fingerprint();
        let expires_at_ms = now.saturating_add(self.ttl_ms);

        let snapshot_id = to_hex(&tagged_mac(
            &self.key,
            "grokptah.source-view.snapshot-id.v1",
            &[
                &revision.to_be_bytes(),
                principal_fingerprint.as_bytes(),
                policy_fingerprint.as_bytes(),
                &now.to_be_bytes(),
            ],
        ))[..32]
            .to_string();

        let mut roots = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let Ok(canonical) = dunce::canonicalize(&candidate.path) else {
                continue;
            };
            let Ok(handle) = RootHandle::open(&canonical) else {
                continue;
            };
            let path_digest = path_digest("grokptah.source-view.root-path.v1", &canonical);
            let identity_digest = handle.identity().digest();
            let token = self.mint_token(
                &snapshot_id,
                index,
                &principal_fingerprint,
                &policy_fingerprint,
                &path_digest,
                &identity_digest,
                expires_at_ms,
            );
            roots.push(ApprovedRoot {
                descriptor: RootDescriptor {
                    token,
                    kind: candidate.kind,
                    label: short_root_label(&canonical),
                    path_digest,
                    identity_digest,
                    run_id: candidate.run_id.clone(),
                },
                handle,
            });
        }

        let snapshot = RootSnapshot {
            snapshot_id: snapshot_id.clone(),
            revision,
            issued_at_ms: now,
            expires_at_ms,
            principal_fingerprint: principal_fingerprint.clone(),
            policy_fingerprint: policy_fingerprint.clone(),
            replay_policy: REPLAY_POLICY,
            roots: roots.iter().map(|root| root.descriptor.clone()).collect(),
        };

        state.entries.insert(
            snapshot_id,
            StoredSnapshot {
                snapshot: snapshot.clone(),
                roots,
                principal_fingerprint,
                policy_fingerprint,
                expires_at_ms,
                revoked: false,
            },
        );
        evict_locked(&mut state, self.max_snapshots);
        snapshot
    }

    /// Resolve a token under the acting authorization context.
    ///
    /// Order matters: cheap structural checks first, then authentication, then
    /// authorization, then the filesystem. A forged token never causes a
    /// filesystem access.
    pub fn resolve(
        &self,
        token: &str,
        context: &AuthorizationContext,
    ) -> Result<ResolvedRoot, SourceViewError> {
        let parsed = ParsedToken::parse(token)?;
        let now = self.clock.now_ms();

        let (root, snapshot_id, revision) = {
            let mut state = self.lock();
            sweep_locked(&mut state, now);

            let stored = state
                .entries
                .get(&parsed.snapshot_id)
                .ok_or(SourceViewError::SnapshotUnknown)?;
            if stored.revoked {
                return Err(SourceViewError::TokenRevoked);
            }
            if now >= stored.expires_at_ms {
                return Err(SourceViewError::TokenExpired);
            }
            let root = stored
                .roots
                .get(parsed.root_index)
                .ok_or(SourceViewError::UnknownRoot)?;

            let expected = self.mint_mac(
                &parsed.snapshot_id,
                parsed.root_index,
                &stored.principal_fingerprint,
                &stored.policy_fingerprint,
                &root.descriptor.path_digest,
                &root.descriptor.identity_digest,
                stored.expires_at_ms,
            );
            if !constant_time_eq(&expected, &parsed.mac) {
                return Err(SourceViewError::TokenSignatureInvalid);
            }
            if context.principal.fingerprint() != stored.principal_fingerprint {
                return Err(SourceViewError::PrincipalMismatch);
            }
            if context.policy.fingerprint() != stored.policy_fingerprint {
                return Err(SourceViewError::PolicyDrift);
            }
            (
                root.clone(),
                stored.snapshot.snapshot_id.clone(),
                stored.snapshot.revision,
            )
        };

        // Action-time identity: the held directory must still be live and
        // unchanged. A worktree discarded and recreated between snapshot and
        // read is a different tree even at the same path and even if the
        // filesystem reuses its inode.
        root.handle.verify()?;

        Ok(ResolvedRoot {
            descriptor: root.descriptor,
            handle: root.handle,
            snapshot_id,
            revision,
        })
    }

    /// Revoke one snapshot. Returns whether it existed.
    pub fn revoke(&self, snapshot_id: &str) -> bool {
        let mut state = self.lock();
        match state.entries.get_mut(snapshot_id) {
            Some(stored) => {
                stored.revoked = true;
                true
            }
            None => false,
        }
    }

    /// Revoke every snapshot issued to a principal. Used when the acting
    /// identity changes: outstanding tokens must not survive a sign-out.
    pub fn revoke_for_principal(&self, principal_fingerprint: &str) -> usize {
        let mut state = self.lock();
        let mut revoked = 0;
        for stored in state.entries.values_mut() {
            if stored.principal_fingerprint == principal_fingerprint && !stored.revoked {
                stored.revoked = true;
                revoked += 1;
            }
        }
        revoked
    }

    /// Drop expired snapshots. Called internally on every issue and resolve;
    /// exposed so a host can also run it on an idle tick.
    pub fn sweep(&self) -> usize {
        let now = self.clock.now_ms();
        let mut state = self.lock();
        sweep_locked(&mut state, now)
    }

    /// Live snapshot count, for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[allow(clippy::too_many_arguments)]
    fn mint_token(
        &self,
        snapshot_id: &str,
        root_index: usize,
        principal_fingerprint: &str,
        policy_fingerprint: &str,
        path_digest: &str,
        identity_digest: &str,
        expires_at_ms: u64,
    ) -> String {
        let mac = self.mint_mac(
            snapshot_id,
            root_index,
            principal_fingerprint,
            policy_fingerprint,
            path_digest,
            identity_digest,
            expires_at_ms,
        );
        format!(
            "{TOKEN_VERSION}.{snapshot_id}.{root_index}.{}",
            to_hex(&mac)
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn mint_mac(
        &self,
        snapshot_id: &str,
        root_index: usize,
        principal_fingerprint: &str,
        policy_fingerprint: &str,
        path_digest: &str,
        identity_digest: &str,
        expires_at_ms: u64,
    ) -> Vec<u8> {
        let full = tagged_mac(
            &self.key,
            "grokptah.source-view.root-token.v1",
            &[
                TOKEN_VERSION.as_bytes(),
                snapshot_id.as_bytes(),
                &(root_index as u64).to_be_bytes(),
                principal_fingerprint.as_bytes(),
                policy_fingerprint.as_bytes(),
                path_digest.as_bytes(),
                identity_digest.as_bytes(),
                &expires_at_ms.to_be_bytes(),
            ],
        );
        full[..MAC_BYTES].to_vec()
    }
}

fn sweep_locked(state: &mut StoreState, now: u64) -> usize {
    let before = state.entries.len();
    state.entries.retain(|_, stored| now < stored.expires_at_ms);
    before - state.entries.len()
}

fn evict_locked(state: &mut StoreState, capacity: usize) {
    while state.entries.len() > capacity {
        let Some(oldest) = state
            .entries
            .iter()
            .min_by_key(|(_, stored)| stored.snapshot.revision)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        state.entries.remove(&oldest);
    }
}

/// A token split into its parts, with every structural rule enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedToken {
    snapshot_id: String,
    root_index: usize,
    mac: Vec<u8>,
}

impl ParsedToken {
    fn parse(token: &str) -> Result<Self, SourceViewError> {
        let mut parts = token.split('.');
        let version = parts.next().ok_or(SourceViewError::TokenMalformed)?;
        let snapshot_id = parts.next().ok_or(SourceViewError::TokenMalformed)?;
        let index = parts.next().ok_or(SourceViewError::TokenMalformed)?;
        let mac = parts.next().ok_or(SourceViewError::TokenMalformed)?;
        if parts.next().is_some() {
            return Err(SourceViewError::TokenMalformed);
        }
        if version != TOKEN_VERSION {
            return Err(SourceViewError::TokenMalformed);
        }
        if snapshot_id.len() != 32 || from_hex(snapshot_id).is_none() {
            return Err(SourceViewError::TokenMalformed);
        }
        // Reject a leading `+`, whitespace, or leading zeros so one root has
        // exactly one token spelling.
        if index.is_empty()
            || !index.bytes().all(|byte| byte.is_ascii_digit())
            || (index.len() > 1 && index.starts_with('0'))
        {
            return Err(SourceViewError::TokenMalformed);
        }
        let root_index: usize = index.parse().map_err(|_| SourceViewError::TokenMalformed)?;
        let mac = from_hex(mac).ok_or(SourceViewError::TokenMalformed)?;
        if mac.len() != MAC_BYTES {
            return Err(SourceViewError::TokenMalformed);
        }
        Ok(Self {
            snapshot_id: snapshot_id.to_string(),
            root_index,
            mac,
        })
    }
}
